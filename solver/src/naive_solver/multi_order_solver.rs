#![allow(dead_code)]

use super::single_pair_settlement::{AmmSwapExactTokensForTokens, SinglePairSettlement};
use crate::settlement::Trade;
use anyhow::{anyhow, Result};
use model::order::{OrderCreation, OrderKind};
use num::{bigint::Sign, BigInt};
use std::collections::HashMap;
use web3::types::{Address, U256};

#[derive(Default)]
struct TokenContext {
    address: Address,
    reserve: BigInt,
    buy_volume: BigInt,
    sell_volume: BigInt,
}

impl TokenContext {
    pub fn is_excess(&self, other: &TokenContext) -> bool {
        &self.reserve * (&other.sell_volume - &other.buy_volume)
            < &other.reserve * (&self.sell_volume - &self.buy_volume)
    }
}

/**
 * Computes a settlement using orders of a single pair and the direct AMM between those tokens.
 * Returns an error if computation fails, orders are not already filtered for a specific token pair, or the reserve information for that
 * pair is not available.
 */
pub fn solve(
    orders: impl Iterator<Item = OrderCreation> + Clone,
    reserves: &HashMap<Address, U256>,
) -> Result<SinglePairSettlement> {
    let (context_a, context_b) = split_into_contexts(orders.clone(), reserves)?;
    let (shortage, excess) = if context_a.is_excess(&context_b) {
        (context_b, context_a)
    } else {
        (context_a, context_b)
    };
    let uniswap_out = compute_uniswap_out(&shortage, &excess);
    let uniswap_in = bigint_to_u256(&compute_uniswap_in(&uniswap_out, &shortage, &excess))?;
    let uniswap_out = bigint_to_u256(&uniswap_out)?;
    let interaction = if uniswap_out > U256::zero() {
        Some(AmmSwapExactTokensForTokens {
            amount_in: uniswap_in,
            amount_out_min: uniswap_out,
            token_in: excess.address,
            token_out: shortage.address,
        })
    } else {
        // TODO(fleupold) set correct clearing prices when supply/demand already match current uniswap price
        None
    };
    // TODO(fleupold) check that all orders comply with the computed price. Otherwise, remove the least favorable excess order and try again.
    Ok(SinglePairSettlement {
        clearing_prices: maplit::hashmap! {
            shortage.address => uniswap_in,
            excess.address => uniswap_out,
        },
        trades: orders.into_iter().map(Trade::fully_matched).collect(),
        interaction,
    })
}

fn split_into_contexts(
    orders: impl Iterator<Item = OrderCreation>,
    reserves: &HashMap<Address, U256>,
) -> Result<(TokenContext, TokenContext)> {
    let mut contexts = HashMap::new();
    for order in orders {
        contexts.entry(order.buy_token).or_insert(TokenContext {
            address: order.buy_token,
            reserve: u256_to_bigint(
                reserves
                    .get(&order.buy_token)
                    .ok_or_else(|| anyhow!("No reserve for token {}", &order.buy_token))?,
            ),
            ..Default::default()
        });
        contexts.entry(order.sell_token).or_insert(TokenContext {
            address: order.sell_token,
            reserve: u256_to_bigint(
                reserves
                    .get(&order.sell_token)
                    .ok_or_else(|| anyhow!("No reserve for token {}", &order.sell_token))?,
            ),
            ..Default::default()
        });
        match order.kind {
            OrderKind::Buy => {
                contexts.get_mut(&order.buy_token).unwrap().buy_volume +=
                    u256_to_bigint(&order.buy_amount)
            }
            OrderKind::Sell => {
                contexts.get_mut(&order.sell_token).unwrap().sell_volume +=
                    u256_to_bigint(&order.sell_amount)
            }
        }
    }
    if contexts.len() != 2 {
        return Err(anyhow!("Orders contain more than two tokens"));
    }
    let mut contexts = contexts.drain().map(|(_, v)| v);
    Ok((contexts.next().unwrap(), contexts.next().unwrap()))
}

/**
 * Given information about the shortage token (the one we need to take from Uniswap) and the excess token (the one we gie to Uniswap), this function
 * computes the exact out_amount required from Uniswap to perfectly match demand and supply at the effective Uniswap price (the one used for that in/out swap).
 *
 * The derivation of this formula is described in https://docs.google.com/document/d/1jS22wxbCqo88fGsqEMZgRQgiAcHlPqxoMw3CJTHst6c/edit
 * It assumes GP fee (φ) to be 1 and Uniswap fee (Φ) to be 0.997
 */
fn compute_uniswap_out(shortage: &TokenContext, excess: &TokenContext) -> BigInt {
    let numerator_minuend = 997 * (&excess.sell_volume - &excess.buy_volume) * &shortage.reserve;
    let numerator_subtrahend =
        1000 * (&shortage.sell_volume - &shortage.buy_volume) * &excess.reserve;
    let denominator = (1000 * &excess.reserve) + (997 * (&excess.sell_volume - &excess.buy_volume));

    (numerator_minuend - numerator_subtrahend) / denominator
}

/**
 * Given the desired amount to receive and the state of the pool, this computes the required amount
 * of tokens to be sent to the pool.
 * Taken from: https://github.com/Uniswap/uniswap-v2-periphery/blob/4123f93278b60bcf617130629c69d4016f9e7584/contracts/libraries/UniswapV2Library.sol#L53
 */
fn compute_uniswap_in(out: &BigInt, shortage: &TokenContext, excess: &TokenContext) -> BigInt {
    1000 * out * &excess.reserve / (997 * (&shortage.reserve - out)) + 1
}

fn u256_to_bigint(input: &U256) -> BigInt {
    let mut bytes = [0; 32];
    input.to_big_endian(&mut bytes);
    BigInt::from_bytes_be(Sign::Plus, &bytes)
}

fn bigint_to_u256(input: &BigInt) -> Result<U256> {
    let (sign, bytes) = input.to_bytes_be();
    if sign == Sign::Minus {
        return Err(anyhow!("Negative BigInt to U256 conversion"));
    }
    if bytes.len() > 32 {
        return Err(anyhow!("BigInt too big for U256 conversion"));
    }
    Ok(U256::from_big_endian(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_wei(base: u32) -> U256 {
        U256::from(base) * U256::from(10).pow(18.into())
    }

    #[test]
    fn finds_clearing_price_with_sell_orders_on_both_sides() {
        let token_a = Address::from_low_u64_be(0);
        let token_b = Address::from_low_u64_be(1);
        let orders = vec![
            OrderCreation {
                sell_token: token_a,
                buy_token: token_b,
                sell_amount: to_wei(40),
                buy_amount: to_wei(30),
                kind: OrderKind::Sell,
                partially_fillable: false,
                ..Default::default()
            },
            OrderCreation {
                sell_token: token_b,
                buy_token: token_a,
                sell_amount: to_wei(100),
                buy_amount: to_wei(90),
                kind: OrderKind::Sell,
                partially_fillable: false,
                ..Default::default()
            },
        ];

        let reserves = maplit::hashmap! {
            token_a => to_wei(1000),
            token_b => to_wei(1000)
        };
        let result = solve(orders.clone().into_iter(), &reserves).unwrap();

        // Make sure the uniswap interaction is using the correct direction
        let interaction = result.interaction.unwrap();
        assert_eq!(interaction.token_in, token_b);
        assert_eq!(interaction.token_out, token_a);

        // Make sure the sell amounts +/- uniswap interaction satisfy min_buy amounts
        assert!(orders[0].sell_amount + interaction.amount_out_min >= orders[1].buy_amount);
        assert!(orders[1].sell_amount - interaction.amount_in > orders[0].buy_amount);

        // Make sure the sell amounts +/- uniswap interaction satisfy expected buy amounts given clearing price
        let price_a = result.clearing_prices.get(&token_a).unwrap();
        let price_b = result.clearing_prices.get(&token_b).unwrap();

        // Multiplying sellAmount with priceA, gives us sell value in "$", divided by priceB gives us value in buy token
        // We should have at least as much to give (sell amount +/- uniswap) as is expected by the buyer
        let expected_buy = orders[0].sell_amount * price_a / price_b;
        assert!(orders[1].sell_amount - interaction.amount_in >= expected_buy);

        let expected_buy = orders[1].sell_amount * price_b / price_a;
        assert!(orders[0].sell_amount + interaction.amount_out_min >= expected_buy);
    }

    #[test]
    fn finds_clearing_price_with_sell_orders_on_one_side() {
        let token_a = Address::from_low_u64_be(0);
        let token_b = Address::from_low_u64_be(1);
        let orders = vec![
            OrderCreation {
                sell_token: token_a,
                buy_token: token_b,
                sell_amount: to_wei(40),
                buy_amount: to_wei(30),
                kind: OrderKind::Sell,
                partially_fillable: false,
                ..Default::default()
            },
            OrderCreation {
                sell_token: token_a,
                buy_token: token_b,
                sell_amount: to_wei(100),
                buy_amount: to_wei(90),
                kind: OrderKind::Sell,
                partially_fillable: false,
                ..Default::default()
            },
        ];

        let reserves = maplit::hashmap! {
            token_a => to_wei(1000),
            token_b => to_wei(1000)
        };
        let result = solve(orders.clone().into_iter(), &reserves).unwrap();

        // Make sure the uniswap interaction is using the correct direction
        let interaction = result.interaction.unwrap();
        assert_eq!(interaction.token_in, token_a);
        assert_eq!(interaction.token_out, token_b);

        // Make sure the sell amounts cover the uniswap in, and min buy amounts are covered by uniswap out
        assert!(orders[0].sell_amount + orders[1].sell_amount >= interaction.amount_in);
        assert!(interaction.amount_out_min >= orders[0].buy_amount + orders[1].buy_amount);

        // Make sure expected buy amounts (given prices) are also covered by uniswap out amounts
        let price_a = result.clearing_prices.get(&token_a).unwrap();
        let price_b = result.clearing_prices.get(&token_b).unwrap();

        let first_expected_buy = orders[0].sell_amount * price_a / price_b;
        let second_expected_buy = orders[1].sell_amount * price_a / price_b;
        assert!(interaction.amount_out_min >= first_expected_buy + second_expected_buy);
    }

    #[test]
    fn finds_clearing_price_with_buy_orders_on_both_sides() {
        let token_a = Address::from_low_u64_be(0);
        let token_b = Address::from_low_u64_be(1);
        let orders = vec![
            OrderCreation {
                sell_token: token_a,
                buy_token: token_b,
                sell_amount: to_wei(40),
                buy_amount: to_wei(30),
                kind: OrderKind::Buy,
                partially_fillable: false,
                ..Default::default()
            },
            OrderCreation {
                sell_token: token_b,
                buy_token: token_a,
                sell_amount: to_wei(100),
                buy_amount: to_wei(90),
                kind: OrderKind::Buy,
                partially_fillable: false,
                ..Default::default()
            },
        ];

        let reserves = maplit::hashmap! {
            token_a => to_wei(1000),
            token_b => to_wei(1000)
        };
        let result = solve(orders.clone().into_iter(), &reserves).unwrap();

        // Make sure the uniswap interaction is using the correct direction
        let interaction = result.interaction.unwrap();
        assert_eq!(interaction.token_in, token_b);
        assert_eq!(interaction.token_out, token_a);

        // Make sure the buy amounts +/- uniswap interaction satisfy max_sell amounts
        assert!(orders[0].sell_amount >= orders[1].buy_amount - interaction.amount_out_min);
        assert!(orders[1].sell_amount >= orders[0].buy_amount + interaction.amount_in);

        // Make sure buy sell amounts +/- uniswap interaction satisfy expected sell amounts given clearing price
        let price_a = result.clearing_prices.get(&token_a).unwrap();
        let price_b = result.clearing_prices.get(&token_b).unwrap();

        // Multiplying buyAmount with priceB, gives us sell value in "$", divided by priceA gives us value in sell token
        // The seller should expect to sell at least as much as we require for the buyer + uniswap.
        let expected_sell = orders[0].buy_amount * price_b / price_a;
        assert!(orders[1].buy_amount - interaction.amount_in <= expected_sell);

        let expected_sell = orders[1].buy_amount * price_a / price_b;
        assert!(orders[0].buy_amount + interaction.amount_out_min <= expected_sell);
    }

    #[test]
    fn finds_clearing_price_with_buy_orders_and_sell_orders() {
        let token_a = Address::from_low_u64_be(0);
        let token_b = Address::from_low_u64_be(1);
        let orders = vec![
            OrderCreation {
                sell_token: token_a,
                buy_token: token_b,
                sell_amount: to_wei(40),
                buy_amount: to_wei(30),
                kind: OrderKind::Buy,
                partially_fillable: false,
                ..Default::default()
            },
            OrderCreation {
                sell_token: token_b,
                buy_token: token_a,
                sell_amount: to_wei(100),
                buy_amount: to_wei(90),
                kind: OrderKind::Sell,
                partially_fillable: false,
                ..Default::default()
            },
        ];

        let reserves = maplit::hashmap! {
            token_a => to_wei(1000),
            token_b => to_wei(1000)
        };
        let result = solve(orders.clone().into_iter(), &reserves).unwrap();

        // Make sure the uniswap interaction is using the correct direction
        let interaction = result.interaction.unwrap();
        assert_eq!(interaction.token_in, token_b);
        assert_eq!(interaction.token_out, token_a);

        // Make sure the buy order's sell amount - uniswap interaction satisfies sell order's limit
        assert!(orders[0].sell_amount >= orders[1].buy_amount - interaction.amount_out_min);

        // Make sure the sell order's buy amount + uniswap interaction satisfies buy order's limit
        assert!(orders[1].buy_amount + interaction.amount_in >= orders[0].sell_amount);

        // Make sure buy sell amounts +/- uniswap interaction satisfy expected sell amounts given clearing price
        let price_a = result.clearing_prices.get(&token_a).unwrap();
        let price_b = result.clearing_prices.get(&token_b).unwrap();

        // Multiplying buy_amount with priceB, gives us sell value in "$", divided by priceA gives us value in sell token
        // The seller should expect to sell at least as much as we require for the buyer + uniswap.
        let expected_sell = orders[0].buy_amount * price_b / price_a;
        assert!(orders[1].buy_amount - interaction.amount_in <= expected_sell);

        // Multiplying sell_amount with priceA, gives us sell value in "$", divided by priceB gives us value in buy token
        // We should have at least as much to give (sell amount + uniswap out) as is expected by the buyer
        let expected_buy = orders[1].sell_amount * price_b / price_a;
        assert!(orders[0].sell_amount + interaction.amount_out_min >= expected_buy);
    }
}
