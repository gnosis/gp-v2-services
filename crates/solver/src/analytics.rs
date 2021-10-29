use crate::settlement::Settlement;
use bigdecimal::Zero;
use model::order::OrderUid;
use num::BigRational;
use shared::conversions::U256Ext;
use std::collections::hash_map::Entry;
use std::collections::HashMap;

/// Record metric with surplus achieved in winning settlement
/// vs that which was unrealized in other feasible solutions.
pub fn num_unsettled_orders_with_better_surplus(
    submitted: &Settlement,
    all: impl Iterator<Item = Settlement>,
) -> usize {
    let submitted_prices = submitted
        .clearing_prices()
        .iter()
        .map(|(token, price)| (token, price.to_big_rational()))
        .collect::<HashMap<_, _>>();
    let submitted_surplus: HashMap<_, _> = submitted
        .trades()
        .iter()
        .map(|trade| {
            (
                trade.order.order_meta_data.uid,
                trade
                    .surplus(
                        &submitted_prices[&trade.order.order_creation.sell_token],
                        &submitted_prices[&trade.order.order_creation.buy_token],
                    )
                    .unwrap_or_else(BigRational::zero),
            )
        })
        .collect();

    let mut best_surplus: HashMap<OrderUid, BigRational> = HashMap::new();
    for settlement in all {
        let trades = settlement.trades();
        let clearing_prices = settlement
            .clearing_prices()
            .iter()
            .map(|(token, price)| (token, price.to_big_rational()))
            .collect::<HashMap<_, _>>();

        for trade in trades {
            let order_id = trade.order.order_meta_data.uid;
            if submitted_surplus.contains_key(&order_id) {
                let surplus = trade
                    .surplus(
                        &clearing_prices[&trade.order.order_creation.sell_token],
                        &clearing_prices[&trade.order.order_creation.buy_token],
                    )
                    .unwrap_or_else(BigRational::zero);
                let entry = best_surplus.entry(order_id);
                match entry {
                    Entry::Occupied(mut entry) => {
                        let value = entry.get_mut();
                        *value = (value.clone()).max(surplus);
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(surplus);
                    }
                }
            }
        }
    }

    let mut count = 0;
    for (order_id, submitted) in submitted_surplus.iter() {
        let best = best_surplus.get(order_id).expect("exists by construction");
        if best > &(BigRational::new(101.into(), 100.into()) * submitted) {
            count += 1;
            tracing::warn!("Found Matched order {:?} whose submission surplus was 1% worse than in another, valid, lower ranked settlement", order_id)
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settlement::{SettlementEncoder, Trade};
    use ethcontract::{H160, U256};
    use maplit::hashmap;
    use model::order::{Order, OrderCreation};

    #[test]
    fn num_unsettled_orders_with_better_surplus_() {
        let buy_token = H160::from_low_u64_be(1);
        let sell_token = H160::from_low_u64_be(2);
        let trade = Trade {
            order: Order {
                order_meta_data: Default::default(),
                order_creation: OrderCreation {
                    sell_token,
                    buy_token,
                    sell_amount: 10.into(),
                    buy_amount: 5.into(),
                    ..Default::default()
                },
            },
            executed_amount: 10.into(),
            ..Default::default()
        };

        let submitted_clearing_prices = hashmap! {
            sell_token => U256::from(1000),
            buy_token => U256::from(1000),
        };
        let similar_clearing_prices = hashmap! {
            sell_token => U256::from(1000),
            buy_token => U256::from(990),
        };
        let better_clearing_prices = hashmap! {
            sell_token => U256::from(1000),
            buy_token => U256::from(989),
        };

        let submitted_settlement = Settlement {
            encoder: SettlementEncoder::with_trades(submitted_clearing_prices, vec![trade.clone()]),
        };
        let better_settlement_1 = Settlement {
            encoder: SettlementEncoder::with_trades(
                better_clearing_prices.clone(),
                vec![trade.clone()],
            ),
        };
        let better_settlement_2 = Settlement {
            encoder: SettlementEncoder::with_trades(better_clearing_prices, vec![trade.clone()]),
        };
        let similar_settlement = Settlement {
            encoder: SettlementEncoder::with_trades(similar_clearing_prices, vec![trade]),
        };

        assert_eq!(
            num_unsettled_orders_with_better_surplus(
                &submitted_settlement,
                vec![
                    submitted_settlement.clone(),
                    // Note that we include two better settlements for the same trade here to
                    // highlight that only this will only be counted once per trade with better surplus
                    // Namely, the best surplus for a trade is compared with submitted surplus.
                    better_settlement_1,
                    better_settlement_2,
                ]
                .into_iter()
            ),
            1
        );

        assert_eq!(
            num_unsettled_orders_with_better_surplus(
                &submitted_settlement,
                vec![submitted_settlement.clone(), similar_settlement].into_iter()
            ),
            0
        );
    }
}
