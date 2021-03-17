use crate::encoding;
use anyhow::Result;
use model::order::{OrderCreation, OrderKind};
use num::{BigRational, CheckedAdd, CheckedDiv, CheckedMul, CheckedSub, Signed};
use primitive_types::{H160, U256};
use shared::conversions::U256Ext;
use std::{collections::HashMap, io::{Cursor, Write}};

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Trade {
    pub order: OrderCreation,
    pub executed_amount: U256,
    pub fee_discount: u16,
}

impl Trade {
    pub fn fully_matched(order: OrderCreation) -> Self {
        let executed_amount = match order.kind {
            model::order::OrderKind::Buy => order.buy_amount,
            model::order::OrderKind::Sell => order.sell_amount,
        };
        Self {
            order,
            executed_amount,
            fee_discount: 0,
        }
    }

    pub fn matched(order: OrderCreation, executed_amount: U256) -> Self {
        Self {
            order,
            executed_amount,
            fee_discount: 0,
        }
    }

    // The difference between the minimum you were willing to buy/maximum you were willing to sell, and what you ended up buying/selling
    pub fn surplus(
        &self,
        sell_token_price: &BigRational,
        buy_token_price: &BigRational,
    ) -> Option<BigRational> {
        match self.order.kind {
            model::order::OrderKind::Buy => buy_order_surplus(
                sell_token_price,
                buy_token_price,
                &self.order.sell_amount.to_big_rational(),
                &self.order.buy_amount.to_big_rational(),
                &self.executed_amount.to_big_rational(),
            ),
            model::order::OrderKind::Sell => sell_order_surplus(
                sell_token_price,
                buy_token_price,
                &self.order.sell_amount.to_big_rational(),
                &self.order.buy_amount.to_big_rational(),
                &self.executed_amount.to_big_rational(),
            ),
        }
    }
}

pub trait Interaction: std::fmt::Debug + Send {
    // TODO: not sure if this should return a result.
    // Write::write returns a result but we know we write to a vector in memory so we know it will
    // never fail. Then the question becomes whether interactions should be allowed to fail encoding
    // for other reasons.
    fn encode(&self, writer: &mut dyn Write) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct Settlement {
    pub clearing_prices: HashMap<H160, U256>,
    pub fee_factor: U256,
    pub trades: Vec<Trade>,
    pub interactions: Vec<Box<dyn Interaction>>,
    pub order_refunds: Vec<()>,
}

impl Settlement {
    pub fn tokens(&self) -> Vec<H160> {
        self.clearing_prices.keys().copied().collect()
    }

    pub fn clearing_prices(&self) -> Vec<U256> {
        self.clearing_prices.values().copied().collect()
    }

    // Returns None if a trade uses a token for which there is no price.
    pub fn encode_trades(&self) -> Option<Vec<u8>> {
        let mut token_index = HashMap::new();
        for (i, token) in self.clearing_prices.keys().enumerate() {
            token_index.insert(token, i as u8);
        }
        let mut bytes = Vec::with_capacity(encoding::TRADE_STRIDE * self.trades.len());
        for trade in &self.trades {
            let order = &trade.order;
            let encoded = encoding::encode_trade(
                &order,
                *token_index.get(&order.sell_token)?,
                *token_index.get(&order.buy_token)?,
                &trade.executed_amount,
                trade.fee_discount,
            );
            bytes.extend_from_slice(&encoded);
        }
        Some(bytes)
    }

    pub fn encode_interactions(&self) -> Result<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        for interaction in &self.interactions {
            interaction.encode(&mut cursor)?;
        }
        Ok(cursor.into_inner())
    }

    fn total_surplus(&self, normalizing_prices: &HashMap<H160, BigRational>) -> Option<BigRational> {
        self.trades.iter().fold(Some(num::zero()), |acc, trade| {
            let sell_token_clearing_price = self.clearing_prices
                .get(&trade.order.sell_token)
                .expect("Solution with trade but without price for sell token")
                .to_big_rational();
            let buy_token_clearing_price = self.clearing_prices
                .get(&trade.order.buy_token)
                .expect("Solution with trade but without price for buy token")
                .to_big_rational();

            let sell_token_external_price = normalizing_prices
                .get(&trade.order.sell_token)
                .expect("Solution with trade but without price for sell token");
            let buy_token_external_price = normalizing_prices
                .get(&trade.order.buy_token)
                .expect("Solution with trade but without price for buy token");

            let surplus = &trade.surplus(&sell_token_clearing_price, &buy_token_clearing_price)?;
            let normalized_surplus = match trade.order.kind {
                OrderKind::Sell => surplus * buy_token_external_price / buy_token_clearing_price,
                OrderKind::Buy => surplus * sell_token_external_price / sell_token_clearing_price,
            };
            acc?.checked_add(&normalized_surplus)
        })
    }

    // Objective is re-computed using external prices.
    fn objective_value_recomputed(
        &self,
        external_prices: &HashMap<H160, BigRational>,
    ) -> Option<BigRational> {
        self.total_surplus(external_prices)
    }

    // For now this computes the total surplus of all EOA trades.
    pub fn objective_value(&self, external_prices: &HashMap<H160, BigRational>) -> BigRational {
        match self.objective_value_recomputed(&external_prices) {
            Some(value) => value,
            None => {
                tracing::error!("Overflow computing objective value for: {:?}", self);
                num::zero()
            }
        }
    }
}

// The difference between what you were willing to sell (executed_amount * limit_price) converted into reference token (multiplied by buy_token_price)
// and what you had to sell denominated in the reference token (executed_amount * buy_token_price)
fn buy_order_surplus(
    sell_token_price: &BigRational,
    buy_token_price: &BigRational,
    sell_amount_limit: &BigRational,
    buy_amount_limit: &BigRational,
    executed_amount: &BigRational,
) -> Option<BigRational> {
    let res = executed_amount
        .checked_mul(sell_amount_limit)?
        .checked_div(buy_amount_limit)?
        .checked_mul(sell_token_price)?
        .checked_sub(&executed_amount.checked_mul(buy_token_price)?)?;
    // Should we simply return 0 when the order fails to satisfy the limit price,
    // or return None as before when we couldn't distinguish between this case from some numerical issue?
    if res.is_negative() {
        None
    } else {
        Some(res)
    }
}

// The difference of your proceeds denominated in the reference token (executed_sell_amount * sell_token_price)
// and what you were minimally willing to receive in buy tokens (executed_sell_amount * limit_price)
// converted to amount in reference token at the effective price (multiplied by buy_token_price)
fn sell_order_surplus(
    sell_token_price: &BigRational,
    buy_token_price: &BigRational,
    sell_amount_limit: &BigRational,
    buy_amount_limit: &BigRational,
    executed_amount: &BigRational,
) -> Option<BigRational> {
    let res = executed_amount.checked_mul(sell_token_price)?.checked_sub(
        &executed_amount
            .checked_mul(buy_amount_limit)?
            .checked_div(sell_amount_limit)?
            .checked_mul(buy_token_price)?,
    )?;
    if res.is_negative() {
        None
    } else {
        Some(res)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::order::OrderKind;
    use num::FromPrimitive;

    #[test]
    pub fn encode_trades_finds_token_index() {
        let token0 = H160::from_low_u64_be(0);
        let token1 = H160::from_low_u64_be(1);
        let order0 = OrderCreation {
            sell_token: token0,
            buy_token: token1,
            ..Default::default()
        };
        let order1 = OrderCreation {
            sell_token: token1,
            buy_token: token0,
            ..Default::default()
        };
        let trade0 = Trade {
            order: order0,
            ..Default::default()
        };
        let trade1 = Trade {
            order: order1,
            ..Default::default()
        };
        let settlement = Settlement {
            clearing_prices: maplit::hashmap! {token0 => 0.into(), token1 => 0.into()},
            trades: vec![trade0, trade1],
            ..Default::default()
        };
        assert!(settlement.encode_trades().is_some());
    }

    // Helper function to save some repeatition below.
    fn r(u: u128) -> BigRational {
        BigRational::from_u128(u).unwrap()
    }

    #[test]
    pub fn objective_value() {
        let token0 = H160::from_low_u64_be(0);
        let token1 = H160::from_low_u64_be(1);

        let order0 = OrderCreation {
            sell_token: token0,
            buy_token: token1,
            sell_amount: 10.into(),
            buy_amount: 9.into(),
            kind: OrderKind::Sell,
            ..Default::default()
        };
        let order1 = OrderCreation {
            sell_token: token1,
            buy_token: token0,
            sell_amount: 10.into(),
            buy_amount: 9.into(),
            kind: OrderKind::Sell,
            ..Default::default()
        };

        let trade0 = Trade {
            order: order0,
            executed_amount: 10.into(),
            ..Default::default()
        };
        let trade1 = Trade {
            order: order1,
            executed_amount: 10.into(),
            ..Default::default()
        };

        // Case where external price vector doesn't influence ranking:

        let clearing_prices0 = maplit::hashmap! {token0 => 1.into(), token1 => 1.into()}; 
        let clearing_prices1 = maplit::hashmap! {token0 => 2.into(), token1 => 2.into()};

        let settlement0 = Settlement {
            clearing_prices: clearing_prices0,
            trades: vec![trade0, trade1],
            ..Default::default()
        };

        let settlement1 = Settlement {
            clearing_prices: clearing_prices1,
            trades: vec![trade0, trade1],
            ..Default::default()
        };

        let external_prices =  maplit::hashmap! {token0 => r(1), token1 => r(1)};
        assert!(settlement0.objective_value(&external_prices) == settlement1.objective_value(&external_prices));

        let external_prices =  maplit::hashmap! {token0 => r(2), token1 => r(1)};
        assert!(settlement0.objective_value(&external_prices) == settlement1.objective_value(&external_prices));

        // Case where external price vector influences ranking:

        let trade0 = Trade {
            order: order0,
            executed_amount: 10.into(),
            ..Default::default()
        };
        let trade1 = Trade {
            order: order1,
            executed_amount: 9.into(),
            ..Default::default()
        };

        let clearing_prices0 = maplit::hashmap! {token0 => 9.into(), token1 => 10.into()}; 

        // Settlement0 gets the following surpluses:
        // trade0: 81 - 81 = 0
        // trade1: 100 - 81 = 19
        let settlement0 = Settlement {
            clearing_prices: clearing_prices0,
            trades: vec![trade0, trade1],
            ..Default::default()
        };

        let trade0 = Trade {
            order: order0,
            executed_amount: 9.into(),
            ..Default::default()
        };
        let trade1 = Trade {
            order: order1,
            executed_amount: 10.into(),
            ..Default::default()
        };

        let clearing_prices1 = maplit::hashmap! {token0 => 10.into(), token1 => 9.into()};

        // Settlement1 gets the following surpluses:
        // trade0: 90 - 72.9 = 17.1
        // trade1: 100 - 100 = 0
        let settlement1 = Settlement {
            clearing_prices: clearing_prices1,
            trades: vec![trade0, trade1],
            ..Default::default()
        };

        // If the external prices of the two tokens is the same, then both settlements are symmetric. 
        let external_prices =  maplit::hashmap! {token0 => r(1), token1 => r(1)};
        assert!(settlement0.objective_value(&external_prices) == settlement1.objective_value(&external_prices));        

        // If the external price of the first token is higher, then the first settlement is preferred.
        let external_prices =  maplit::hashmap! {token0 => r(2), token1 => r(1)};

        // Settlement0 gets the following normalized surpluses:
        // trade0: 0
        // trade1: 19 * 2 / 10 = 3.8

        // Surpluses of settlement1 get normalized as:
        // trade0: 17.1 * 1 / 9 = 1.9
        // trade1: 0

        assert!(settlement0.objective_value(&external_prices) > settlement1.objective_value(&external_prices));

        // If the external price of the second token is higher, then the second settlement is preferred.
        // (swaps above normalized surpluses of settlement0 and settlement1)
        let external_prices =  maplit::hashmap! {token0 => r(1), token1 => r(2)};

        assert!(settlement0.objective_value(&external_prices) < settlement1.objective_value(&external_prices));
    }

    #[test]
    #[allow(clippy::just_underscores_and_digits)]
    fn test_buy_order_surplus() {
        // Two goods are worth the same (100 each). If we were willing to pay up to 60 to receive 50,
        // but ended paying the price (1) we have a surplus of 10 sell units, so a total surplus of 1000.

        assert_eq!(
            buy_order_surplus(&r(100), &r(100), &r(60), &r(50), &r(50)),
            Some(r(1000))
        );

        // If our trade got only half filled, we only get half the surplus
        assert_eq!(
            buy_order_surplus(&r(100), &r(100), &r(60), &r(50), &r(25)),
            Some(r(500))
        );

        // No surplus if trade is not at all filled
        assert_eq!(
            buy_order_surplus(&r(100), &r(100), &r(60), &r(50), &r(0)),
            Some(r(0))
        );

        // No surplus if trade is filled at limit
        assert_eq!(buy_order_surplus(&r(100), &r(100), &r(50), &r(50), &r(50)), Some(r(0)));

        // Arithmetic error when limit price not respected
        assert_eq!(buy_order_surplus(&r(100), &r(100), &r(40), &r(50), &r(50)), None);

        // Sell Token worth twice as much as buy token. If we were willing to sell at parity, we will
        // have a surplus of 50% of tokens, worth 200 each.
        assert_eq!(
            buy_order_surplus(&r(200), &r(100), &r(50), &r(50), &r(50)),
            Some(r(5000))
        );

        // Buy Token worth twice as much as sell token. If we were willing to sell at 3:1, we will
        // have a surplus of 20 sell tokens, worth 100 each.
        assert_eq!(
            buy_order_surplus(&r(100), &r(200), &r(60), &r(20), &r(20)),
            Some(r(2000))
        );
    }

    #[test]
    #[allow(clippy::just_underscores_and_digits)]
    fn test_sell_order_surplus() {
        // Two goods are worth the same (100 each). If we were willing to receive as little as 40,
        // but ended paying the price (1) we have a surplus of 10 bought units, so a total surplus of 1000.

        assert_eq!(
            sell_order_surplus(&r(100), &r(100), &r(50), &r(40), &r(50)),
            Some(r(1000))
        );

        // If our trade got only half filled, we only get half the surplus
        assert_eq!(
            sell_order_surplus(&r(100), &r(100), &r(50), &r(40), &r(25)),
            Some(r(500))
        );

        // No surplus if trade is not at all filled
        assert_eq!(
            sell_order_surplus(&r(100), &r(100), &r(50), &r(40), &r(0)),
            Some(r(0))
        );

        // No surplus if trade is filled at limit
        assert_eq!(sell_order_surplus(&r(100), &r(100), &r(50), &r(50), &r(50)), Some(r(0)));

        // Arithmetic error when limit price not respected
        assert_eq!(sell_order_surplus(&r(100), &r(100), &r(50), &r(60), &r(50)), None);

        // Sell token worth twice as much as buy token. If we were willing to buy at parity, we will
        // have a surplus of 100% of buy tokens, worth 100 each.
        assert_eq!(
            sell_order_surplus(&r(200), &r(100), &r(50), &r(50), &r(50)),
            Some(r(5000))
        );

        // Buy Token worth twice as much as sell token. If we were willing to buy at 3:1, we will
        // have a surplus of 10 sell tokens, worth 200 each.
        assert_eq!(
            buy_order_surplus(&r(100), &r(200), &r(60), &r(20), &r(20)),
            Some(r(2000))
        );
    }
}
