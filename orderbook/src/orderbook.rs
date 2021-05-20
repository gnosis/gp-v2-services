use crate::{
    account_balances::BalanceFetching,
    database::OrderFilter,
    database::{Database, InsertionError},
    fee::{EthAwareMinFeeCalculator, MinFeeCalculating},
};
use anyhow::Result;
use chrono::Utc;
use futures::TryStreamExt;
use model::order::{OrderCancellation, OrderCreationPayload};
use model::{
    order::{Order, OrderUid},
    DomainSeparator,
};
use primitive_types::{H160, U256};
use shared::{bad_token::BadTokenDetecting, maintenance::Maintaining, time::now_in_epoch_seconds};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

#[derive(Debug, Eq, PartialEq)]
pub enum AddOrderResult {
    Added(OrderUid),
    WrongOwner(H160),
    DuplicatedOrder,
    InvalidSignature,
    Forbidden,
    MissingOrderData,
    InsufficientValidTo,
    InsufficientFunds,
    InsufficientFee,
    UnsupportedToken(H160),
}

#[derive(Debug)]
pub enum OrderCancellationResult {
    Cancelled,
    InvalidSignature,
    WrongOwner,
    OrderNotFound,
}

pub struct Orderbook {
    domain_separator: DomainSeparator,
    database: Database,
    balance_fetcher: Box<dyn BalanceFetching>,
    fee_validator: Arc<EthAwareMinFeeCalculator>,
    min_order_validity_period: Duration,
    bad_token_detector: Arc<dyn BadTokenDetecting>,
}

impl Orderbook {
    pub fn new(
        domain_separator: DomainSeparator,
        database: Database,
        balance_fetcher: Box<dyn BalanceFetching>,
        fee_validator: Arc<EthAwareMinFeeCalculator>,
        min_order_validity_period: Duration,
        bad_token_detector: Arc<dyn BadTokenDetecting>,
    ) -> Self {
        Self {
            domain_separator,
            database,
            balance_fetcher,
            fee_validator,
            min_order_validity_period,
            bad_token_detector,
        }
    }

    pub async fn add_order(&self, payload: OrderCreationPayload) -> Result<AddOrderResult> {
        let order = payload.order_creation;
        if order.valid_to
            < shared::time::now_in_epoch_seconds() + self.min_order_validity_period.as_secs() as u32
        {
            return Ok(AddOrderResult::InsufficientValidTo);
        }
        if !self
            .fee_validator
            .is_valid_fee(order.sell_token, order.fee_amount)
            .await
        {
            return Ok(AddOrderResult::InsufficientFee);
        }
        let order = match Order::from_order_creation(order, &self.domain_separator) {
            Some(order) => order,
            None => return Ok(AddOrderResult::InvalidSignature),
        };
        if matches!(payload.from, Some(from) if from != order.order_meta_data.owner) {
            return Ok(AddOrderResult::WrongOwner(order.order_meta_data.owner));
        }

        for &token in &[
            order.order_creation.sell_token,
            order.order_creation.buy_token,
        ] {
            if !self.bad_token_detector.detect(token).await?.is_good() {
                return Ok(AddOrderResult::UnsupportedToken(token));
            }
        }

        let min_balance = match minimum_balance(&order) {
            Some(amount) => amount,
            None => return Ok(AddOrderResult::InsufficientFunds),
        };
        if !self
            .balance_fetcher
            .can_transfer(
                order.order_creation.sell_token,
                order.order_meta_data.owner,
                min_balance,
            )
            .await
            .unwrap_or(false)
        {
            return Ok(AddOrderResult::InsufficientFunds);
        }

        match self.database.insert_order(&order).await {
            Err(InsertionError::DuplicatedRecord) => return Ok(AddOrderResult::DuplicatedOrder),
            Err(InsertionError::DbError(err)) => return Err(err.into()),
            _ => (),
        }
        self.balance_fetcher
            .register(order.order_meta_data.owner, order.order_creation.sell_token)
            .await;
        Ok(AddOrderResult::Added(order.order_meta_data.uid))
    }

    pub async fn cancel_order(
        &self,
        cancellation: OrderCancellation,
    ) -> Result<OrderCancellationResult> {
        // TODO - Would like to use get_order_by_uid, but not implemented on self
        let orders = self
            .get_orders(&OrderFilter {
                uid: Some(cancellation.order_uid),
                ..Default::default()
            })
            .await?;
        // Could be that order doesn't exist and is not fetched.
        let order = match orders.first() {
            Some(order) => order,
            None => return Ok(OrderCancellationResult::OrderNotFound),
        };

        match cancellation.validate(&self.domain_separator) {
            Some(signer) => {
                if signer == order.order_meta_data.owner {
                    // order is already known to exist in DB at this point!
                    self.database
                        .cancel_order(&order.order_meta_data.uid, Utc::now())
                        .await?;
                    Ok(OrderCancellationResult::Cancelled)
                } else {
                    Ok(OrderCancellationResult::WrongOwner)
                }
            }
            None => Ok(OrderCancellationResult::InvalidSignature),
        }
    }

    pub async fn get_orders(&self, filter: &OrderFilter) -> Result<Vec<Order>> {
        let mut orders = self.database.orders(filter).try_collect::<Vec<_>>().await?;
        let balances =
            track_and_get_balances(self.balance_fetcher.as_ref(), orders.as_slice()).await;
        // The meaning of the available balance field is different depending on whether we return
        // orders for the solver or the frontend. For the frontend (else case) balances are always
        // actual balances but for the solver there is custom logic to decide which orders get
        // prioritized when a user's balance is too small to cover all of their orders.
        // We can hopefully resolve this when we have a custom struct for orders in the
        // get_solver_orders route and a custom endpoint to query user balances for the frontend.
        set_available_balances(orders.as_mut_slice(), &balances);
        if filter.exclude_insufficient_balance {
            orders = solvable_orders(orders, &balances);
        }
        if filter.exclude_unsupported_tokens {
            orders = filter_unsupported_tokens(orders, self.bad_token_detector.as_ref()).await?;
        }
        Ok(orders)
    }

    pub async fn get_solvable_orders(&self) -> Result<Vec<Order>> {
        let filter = OrderFilter {
            min_valid_to: now_in_epoch_seconds() + self.min_order_validity_period.as_secs() as u32,
            exclude_fully_executed: true,
            exclude_invalidated: true,
            exclude_insufficient_balance: true,
            exclude_unsupported_tokens: true,
            ..Default::default()
        };
        self.get_orders(&filter).await
    }
}

#[async_trait::async_trait]
impl Maintaining for Orderbook {
    async fn run_maintenance(&self) -> Result<()> {
        self.balance_fetcher.update().await;
        Ok(())
    }
}

async fn filter_unsupported_tokens(
    mut orders: Vec<Order>,
    bad_token: &dyn BadTokenDetecting,
) -> Result<Vec<Order>> {
    // Can't use normal `retain` or `filter` because the bad token detection is async. So either
    // this manual iteration or conversion to stream.
    let mut index = 0;
    'outer: while index < orders.len() {
        for token in orders[index].order_creation.token_pair().unwrap() {
            if !bad_token.detect(token).await?.is_good() {
                orders.swap_remove(index);
                continue 'outer;
            }
        }
        index += 1;
    }
    Ok(orders)
}

// Make sure the balance fetcher tracks all balances for user, sell token combinations in these
// orders and returns said balances.
async fn track_and_get_balances(
    fetcher: &dyn BalanceFetching,
    orders: &[Order],
) -> HashMap<(H160, H160), U256> {
    let mut balances = HashMap::<(H160, H160), U256>::new();
    let mut untracked = HashSet::<(H160, H160)>::new();
    for order in orders {
        let key = (order.order_meta_data.owner, order.order_creation.sell_token);
        match fetcher.get_balance(key.0, key.1) {
            Some(balance) => {
                balances.insert(key, balance);
            }
            None => {
                untracked.insert(key);
            }
        }
    }
    fetcher
        .register_many(untracked.iter().cloned().collect())
        .await;
    balances.extend(untracked.into_iter().filter_map(|key| {
        fetcher
            .get_balance(key.0, key.1)
            .map(|balance| (key, balance))
    }));
    balances
}

fn set_available_balances(orders: &mut [Order], balances: &HashMap<(H160, H160), U256>) {
    for order in orders.iter_mut() {
        let key = &(order.order_meta_data.owner, order.order_creation.sell_token);
        order.order_meta_data.available_balance = balances.get(key).cloned();
    }
}

// The order book has to make a choice for which orders to include when a user has multiple orders
// selling the same token but not enough balance for all of them.
// Assumes balance fetcher is already tracking all balances.
fn solvable_orders(mut orders: Vec<Order>, balances: &HashMap<(H160, H160), U256>) -> Vec<Order> {
    let mut orders_map = HashMap::<(H160, H160), Vec<Order>>::new();
    orders.sort_by_key(|order| order.order_meta_data.creation_date);
    for order in orders {
        let key = (order.order_meta_data.owner, order.order_creation.sell_token);
        orders_map.entry(key).or_default().push(order);
    }

    let mut result = Vec::new();
    for (key, orders) in orders_map {
        let mut remaining_balance = match balances.get(&key) {
            Some(balance) => *balance,
            None => continue,
        };
        for order in orders {
            // TODO: This is overly pessimistic for partially filled orders where the needed balance
            // is lower. For partially fillable orders that cannot be fully filled because of the
            // balance we could also give them as much balance as possible instead of skipping. For
            // that we first need a way to communicate this to the solver. We could repurpose
            // availableBalance for this.
            let needed_balance = match order
                .order_creation
                .sell_amount
                .checked_add(order.order_creation.fee_amount)
            {
                Some(balance) => balance,
                None => continue,
            };
            if let Some(balance) = remaining_balance.checked_sub(needed_balance) {
                remaining_balance = balance;
                result.push(order);
            }
        }
    }
    result
}

// Mininum balance user must have in sell token for order to be accepted. None if no balance is
// sufficient.
fn minimum_balance(order: &Order) -> Option<U256> {
    if order.order_creation.partially_fillable {
        Some(U256::from(1))
    } else {
        order
            .order_creation
            .sell_amount
            .checked_add(order.order_creation.fee_amount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_balances::MockBalanceFetching;
    use chrono::{DateTime, NaiveDateTime};
    use ethcontract::H160;
    use futures::FutureExt;
    use maplit::hashmap;
    use mockall::{predicate::eq, Sequence};
    use model::order::{OrderBuilder, OrderCreation, OrderMetaData};
    use shared::bad_token::list_based::ListBasedDetector;

    #[tokio::test]
    async fn track_and_get_balances_() {
        let mut balance_fetcher = MockBalanceFetching::new();

        let a_sell_token = H160::from_low_u64_be(2);
        let a_balance = 100.into();

        let another_sell_token = H160::from_low_u64_be(3);
        let another_balance = 200.into();

        let orders = vec![
            Order {
                order_creation: OrderCreation {
                    sell_token: a_sell_token,
                    ..Default::default()
                },
                ..Default::default()
            },
            Order {
                order_creation: OrderCreation {
                    sell_token: another_sell_token,
                    ..Default::default()
                },
                ..Default::default()
            },
        ];
        let owner = orders[0].order_meta_data.owner;

        balance_fetcher
            .expect_get_balance()
            .with(eq(owner), eq(a_sell_token))
            .return_const(Some(a_balance));

        // Not having a balance for the second order, should trigger a register_many only for this token
        let mut seq = Sequence::new();
        balance_fetcher
            .expect_get_balance()
            .with(eq(owner), eq(another_sell_token))
            .times(1)
            .in_sequence(&mut seq)
            .return_const(None);

        balance_fetcher
            .expect_register_many()
            .with(eq(vec![(owner, another_sell_token)]))
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_| ());

        // Once registered, we can return the balance
        balance_fetcher
            .expect_get_balance()
            .with(eq(owner), eq(another_sell_token))
            .times(1)
            .in_sequence(&mut seq)
            .return_const(Some(another_balance));

        let balances = track_and_get_balances(&balance_fetcher, orders.as_slice()).await;
        assert_eq!(
            balances,
            hashmap! {
                (owner, a_sell_token) => a_balance,
                (owner, another_sell_token) => another_balance
            }
        );
    }

    #[tokio::test]
    async fn filters_insufficient_balances() {
        let mut balance_fetcher = MockBalanceFetching::new();
        balance_fetcher
            .expect_get_balance()
            .return_const(Some(10.into()));

        let mut orders = vec![
            Order {
                order_creation: OrderCreation {
                    sell_amount: 3.into(),
                    fee_amount: 3.into(),
                    ..Default::default()
                },
                order_meta_data: OrderMetaData {
                    creation_date: DateTime::from_utc(NaiveDateTime::from_timestamp(2, 0), Utc),
                    ..Default::default()
                },
            },
            Order {
                order_creation: OrderCreation {
                    sell_amount: 2.into(),
                    fee_amount: 2.into(),
                    ..Default::default()
                },
                order_meta_data: OrderMetaData {
                    creation_date: DateTime::from_utc(NaiveDateTime::from_timestamp(0, 0), Utc),
                    ..Default::default()
                },
            },
        ];

        let balances = hashmap! {Default::default() => U256::from(9)};
        let orders_ = solvable_orders(orders.clone(), &balances);
        // First order has higher timestamp so it isn't picked.
        assert_eq!(orders_, orders[1..]);
        orders[1].order_meta_data.creation_date =
            DateTime::from_utc(NaiveDateTime::from_timestamp(3, 0), Utc);
        let orders_ = solvable_orders(orders.clone(), &balances);
        assert_eq!(orders_, orders[..1]);
    }

    #[test]
    fn filter_unsupported_tokens_() {
        let token0 = H160::from_low_u64_le(0);
        let token1 = H160::from_low_u64_le(1);
        let token2 = H160::from_low_u64_le(2);
        let bad_token = ListBasedDetector::deny_list(vec![token0]);
        let orders = vec![
            OrderBuilder::default()
                .with_sell_token(token0)
                .with_buy_token(token1)
                .build(),
            OrderBuilder::default()
                .with_sell_token(token1)
                .with_buy_token(token2)
                .build(),
            OrderBuilder::default()
                .with_sell_token(token0)
                .with_buy_token(token2)
                .build(),
        ];
        let result = filter_unsupported_tokens(orders.clone(), &bad_token)
            .now_or_never()
            .unwrap()
            .unwrap();
        assert_eq!(result, &orders[1..2]);
    }
}
