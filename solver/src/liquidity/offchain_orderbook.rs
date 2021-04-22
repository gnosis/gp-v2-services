use crate::interactions::UnwrapWethInteraction;
use crate::orderbook::OrderBookApi;
use crate::settlement::SettlementEncoder;
use anyhow::{anyhow, Context, Result};
use contracts::WETH9;
use ethcontract::H160;
use model::order::{Order, OrderKind};
use primitive_types::U256;
use std::{collections::HashMap, sync::Arc};

use super::{LimitOrder, SettlementHandling};

const BUY_ETH_ADDRESS: H160 = H160([0xee; 20]);

impl OrderBookApi {
    /// Returns a list of limit orders coming from the offchain orderbook API
    pub async fn get_liquidity(&self) -> Result<Vec<LimitOrder>> {
        Ok(self
            .get_orders()
            .await
            .context("failed to get orderbook")?
            .into_iter()
            .map(|order| normalize_limit_order(order, self.get_native_token()))
            .collect())
    }
}

struct OrderSettlementHandler {
    #[allow(dead_code)]
    native_token: WETH9,
    order: Order,
}

pub fn normalize_limit_order(order: Order, native_token: WETH9) -> LimitOrder {
    LimitOrder {
        id: order.order_meta_data.uid.to_string(),
        sell_token: order.order_creation.sell_token,
        // TODO handle ETH buy token address (0xe...e) by making the handler include an WETH.unwrap() interaction
        buy_token: order.order_creation.buy_token,
        // TODO discount previously executed sell amount
        sell_amount: order.order_creation.sell_amount,
        buy_amount: order.order_creation.buy_amount,
        kind: order.order_creation.kind,
        partially_fillable: order.order_creation.partially_fillable,
        settlement_handling: Arc::new(OrderSettlementHandler {
            order,
            native_token,
        }),
    }
}

impl SettlementHandling<LimitOrder> for OrderSettlementHandler {
    fn encode(&self, executed_amount: U256, encoder: &mut SettlementEncoder) -> Result<()> {
        if self.order.order_creation.buy_token == BUY_ETH_ADDRESS {
            let interaction = compute_unwrap_interaction(
                encoder.clearing_prices(),
                &self.order,
                executed_amount,
                self.native_token.clone(),
            )?;

            encoder.add_trade(self.order.clone(), executed_amount)?;

            encoder.add_unwrap(interaction);
        } else {
            encoder.add_trade(self.order.clone(), executed_amount)?;
        }

        Ok(())
    }
}

fn compute_unwrap_interaction(
    clearing_prices: &HashMap<H160, U256>,
    order: &Order,
    executed_amount: U256,
    weth: WETH9,
) -> Result<UnwrapWethInteraction> {
    let sell_price = *clearing_prices
        .get(&order.order_creation.sell_token)
        .ok_or_else(|| anyhow!("sell price not available"))?;
    let buy_price = *clearing_prices
        .get(&order.order_creation.buy_token)
        .ok_or_else(|| anyhow!("buy price not available"))?;
    let amount = executed_buy_amount(
        order,
        executed_amount,
        Price {
            sell_price,
            buy_price,
        },
    )
    .ok_or_else(|| anyhow!("cannot compute executed buy amount"))?;
    Ok(UnwrapWethInteraction { weth, amount })
}

struct Price {
    sell_price: U256,
    buy_price: U256,
}

fn executed_buy_amount(order: &Order, executed_amount: U256, price: Price) -> Option<U256> {
    Some(match order.order_creation.kind {
        OrderKind::Sell => executed_amount
            .checked_mul(price.sell_price)?
            .checked_div(price.buy_price)?,
        OrderKind::Buy => executed_amount,
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::interactions::dummy_web3;
    use crate::settlement::tests::assert_settlement_encoded_with;
    use maplit::hashmap;
    use model::order::OrderCreation;

    #[test]
    fn adds_unwrap_interaction_for_sell_order_with_eth_flag() {
        let native_token_address = H160([0x42; 20]);
        let sell_token = H160([0x21; 20]);
        let native_token = WETH9::at(&dummy_web3::dummy_web3(), native_token_address);

        let executed_amount = U256::from(1337);
        let prices = hashmap! {
            BUY_ETH_ADDRESS => U256::from(100),
            sell_token => U256::from(200),
        };
        let executed_buy_amount = U256::from(2 * 1337);
        let order = Order {
            order_creation: OrderCreation {
                buy_token: BUY_ETH_ADDRESS,
                sell_token,
                kind: OrderKind::Sell,
                ..Default::default()
            },
            ..Default::default()
        };
        println!("{}", order.order_creation.buy_token);

        let order_settlement_handler = OrderSettlementHandler {
            order: order.clone(),
            native_token: native_token.clone(),
        };

        assert_settlement_encoded_with(
            prices,
            order_settlement_handler,
            executed_amount,
            |encoder| {
                assert!(encoder.add_trade(order, executed_amount).is_ok());
                encoder.add_unwrap(UnwrapWethInteraction {
                    weth: native_token,
                    amount: executed_buy_amount,
                });
            },
        );
    }

    #[test]
    fn adds_unwrap_interaction_for_buy_order_with_eth_flag() {
        let native_token_address = H160([0x42; 20]);
        let sell_token = H160([0x21; 20]);
        let native_token = WETH9::at(&dummy_web3::dummy_web3(), native_token_address);
        let executed_amount = U256::from(1337);
        let prices = hashmap! {
            BUY_ETH_ADDRESS => U256::from(1),
            sell_token => U256::from(2),
        };
        let order = Order {
            order_creation: OrderCreation {
                buy_token: BUY_ETH_ADDRESS,
                sell_token,
                kind: OrderKind::Buy,
                ..Default::default()
            },
            ..Default::default()
        };
        println!("{}", order.order_creation.buy_token);

        let order_settlement_handler = OrderSettlementHandler {
            order: order.clone(),
            native_token: native_token.clone(),
        };

        assert_settlement_encoded_with(
            prices,
            order_settlement_handler,
            executed_amount,
            |encoder| {
                assert!(encoder.add_trade(order, executed_amount).is_ok());
                encoder.add_unwrap(UnwrapWethInteraction {
                    weth: native_token,
                    amount: executed_amount,
                });
            },
        );
    }

    #[test]
    fn does_not_add_unwrap_interaction_for_order_without_eth_flag() {
        let native_token_address = H160([0x42; 20]);
        let sell_token = H160([0x21; 20]);
        let native_token = WETH9::at(&dummy_web3::dummy_web3(), native_token_address);
        let not_buy_eth_address = H160([0xff; 20]);
        assert_ne!(not_buy_eth_address, BUY_ETH_ADDRESS);

        let executed_amount = U256::from(1337);
        let prices = hashmap! {
            not_buy_eth_address => U256::from(100),
            sell_token => U256::from(200),
        };
        let order = Order {
            order_creation: OrderCreation {
                buy_token: not_buy_eth_address,
                sell_token,
                ..Default::default()
            },
            ..Default::default()
        };

        let order_settlement_handler = OrderSettlementHandler {
            order: order.clone(),
            native_token,
        };

        assert_settlement_encoded_with(
            prices,
            order_settlement_handler,
            executed_amount,
            |encoder| {
                assert!(encoder.add_trade(order, executed_amount).is_ok());
            },
        );
    }
}
