use crate::encoding;
use anyhow::Result;
use model::order::OrderCreation;
use num::{BigRational, CheckedAdd, CheckedDiv, CheckedMul, CheckedSub, FromPrimitive, Signed};
use primitive_types::{H160, U256};
use shared::conversions::U256Ext;
use std::{
    collections::HashMap,
    io::{Cursor, Write},
};

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

    fn total_surplus(&self, prices: &HashMap<H160, BigRational>) -> Option<BigRational> {
        self.trades.iter().fold(Some(num::zero()), |acc, trade| {
            let sell_token_external_price = prices
                .get(&trade.order.sell_token)
                .expect("Solution with trade but without price for sell token");
            let buy_token_external_price = prices
                .get(&trade.order.buy_token)
                .expect("Solution with trade but without price for buy token");
            acc?.checked_add(&trade.surplus(sell_token_external_price, buy_token_external_price)?)
        })
    }

    // Objective is re-computed using external prices.
    fn objective_value_v1(
        &self,
        external_prices: &HashMap<H160, BigRational>,
    ) -> Option<BigRational> {
        self.total_surplus(external_prices)
    }

    // Objective is scaled by (harmonic) mean of external/found price ratio.
    #[allow(dead_code)]
    fn objective_value_v2(
        &self,
        external_prices: &HashMap<H160, BigRational>,
    ) -> Option<BigRational> {
        let clearing_prices = self
            .clearing_prices
            .iter()
            .map(|tp| (*tp.0, tp.1.to_big_rational()))
            .collect();
        let unscaled_obj = self.total_surplus(&clearing_prices)?;

        // scale = nr_tokens / (p_1/p'_1 + ... + p_n/p'_n)
        let numerator: BigRational = BigRational::from_usize(self.tokens().len()).unwrap();
        let denominator: BigRational =
            clearing_prices
                .iter()
                .fold(Some(num::zero()), |acc: Option<BigRational>, value| {
                    let external_price = external_prices.get(value.0)?;
                    acc?.checked_add(&value.1.checked_div(external_price)?)
                })?;

        unscaled_obj
            .checked_mul(&numerator)?
            .checked_div(&denominator)
    }

    // For now this computes the total surplus of all EOA trades.
    pub fn objective_value(&self, external_prices: &HashMap<H160, BigRational>) -> BigRational {
        match self.objective_value_v1(&external_prices) {
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
    // Shouldn we simply return 0 when the order fails to satisfy the limit price,
    // or return None as before when we couldn't distinguish between this case from some numerical issue.
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

    #[test]
    #[allow(clippy::just_underscores_and_digits)]
    fn test_buy_order_surplus() {
        // Two goods are worth the same (100 each). If we were willing to pay up to 60 to receive 50,
        // but ended paying the price (1) we have a surplus of 10 sell units, so a total surplus of 1000.

        // I hope there is a better way :)
        let _5000 = BigRational::from_u32(5000).unwrap();
        let _2000 = BigRational::from_u32(2000).unwrap();
        let _1000 = BigRational::from_u32(1000).unwrap();
        let _500 = BigRational::from_u32(500).unwrap();
        let _200 = BigRational::from_u32(200).unwrap();
        let _100 = BigRational::from_u32(100).unwrap();
        let _60 = BigRational::from_u32(60).unwrap();
        let _50 = BigRational::from_u32(50).unwrap();
        let _40 = BigRational::from_u32(40).unwrap();
        let _25 = BigRational::from_u32(25).unwrap();
        let _20 = BigRational::from_u32(20).unwrap();
        let _0 = BigRational::from_u32(0).unwrap();

        assert_eq!(
            buy_order_surplus(&_100, &_100, &_60, &_50, &_50),
            Some(_1000)
        );

        // If our trade got only half filled, we only get half the surplus
        assert_eq!(
            buy_order_surplus(&_100, &_100, &_60, &_50, &_25),
            Some(_500)
        );

        // No surplus if trade is not at all filled
        assert_eq!(
            buy_order_surplus(&_100, &_100, &_60, &_50, &_0),
            Some(_0.clone())
        );

        // No surplus if trade is filled at limit
        assert_eq!(buy_order_surplus(&_100, &_100, &_50, &_50, &_50), Some(_0));

        // Arithmetic error when limit price not respected
        assert_eq!(buy_order_surplus(&_100, &_100, &_40, &_50, &_50), None);

        // Sell Token worth twice as much as buy token. If we were willing to sell at parity, we will
        // have a surplus of 50% of tokens, worth 200 each.
        assert_eq!(
            buy_order_surplus(&_200, &_100, &_50, &_50, &_50),
            Some(_5000)
        );

        // Buy Token worth twice as much as sell token. If we were willing to sell at 3:1, we will
        // have a surplus of 20 sell tokens, worth 100 each.
        assert_eq!(
            buy_order_surplus(&_100, &_200, &_60, &_20, &_20),
            Some(_2000)
        );
    }

    #[test]
    #[allow(clippy::just_underscores_and_digits)]
    fn test_sell_order_surplus() {
        // Two goods are worth the same (100 each). If we were willing to receive as little as 40,
        // but ended paying the price (1) we have a surplus of 10 bought units, so a total surplus of 1000.

        let _5000 = BigRational::from_u32(5000).unwrap();
        let _2000 = BigRational::from_u32(2000).unwrap();
        let _1000 = BigRational::from_u32(1000).unwrap();
        let _500 = BigRational::from_u32(500).unwrap();
        let _200 = BigRational::from_u32(200).unwrap();
        let _100 = BigRational::from_u32(100).unwrap();
        let _60 = BigRational::from_u32(60).unwrap();
        let _50 = BigRational::from_u32(50).unwrap();
        let _40 = BigRational::from_u32(40).unwrap();
        let _25 = BigRational::from_u32(25).unwrap();
        let _20 = BigRational::from_u32(20).unwrap();
        let _0 = BigRational::from_u32(0).unwrap();

        assert_eq!(
            sell_order_surplus(&_100, &_100, &_50, &_40, &_50),
            Some(_1000)
        );

        // If our trade got only half filled, we only get half the surplus
        assert_eq!(
            sell_order_surplus(&_100, &_100, &_50, &_40, &_25),
            Some(_500)
        );

        // No surplus if trade is not at all filled
        assert_eq!(
            sell_order_surplus(&_100, &_100, &_50, &_40, &_0),
            Some(_0.clone())
        );

        // No surplus if trade is filled at limit
        assert_eq!(sell_order_surplus(&_100, &_100, &_50, &_50, &_50), Some(_0));

        // Arithmetic error when limit price not respected
        assert_eq!(sell_order_surplus(&_100, &_100, &_50, &_60, &_50), None);

        // Sell token worth twice as much as buy token. If we were willing to buy at parity, we will
        // have a surplus of 100% of buy tokens, worth 100 each.
        assert_eq!(
            sell_order_surplus(&_200, &_100, &_50, &_50, &_50),
            Some(_5000)
        );

        // Buy Token worth twice as much as sell token. If we were willing to buy at 3:1, we will
        // have a surplus of 10 sell tokens, worth 200 each.
        assert_eq!(
            buy_order_surplus(&_100, &_200, &_60, &_20, &_20),
            Some(_2000)
        );
    }
}
