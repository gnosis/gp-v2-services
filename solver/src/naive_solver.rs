mod multi_order_solver;

use crate::{
    liquidity::{AmmOrder, LimitOrder, Liquidity},
    settlement::Settlement,
    solver::Solver,
    uniswap_solver::extract_deepest_amm_liquidity,
};
use anyhow::Result;
use model::TokenPair;
use std::{collections::HashMap, fmt};

pub struct NaiveSolver;

#[async_trait::async_trait]
impl Solver for NaiveSolver {
    async fn solve(
        &self,
        liquidity: Vec<Liquidity>,
        _gas_price: f64,
    ) -> Result<Option<Settlement>> {
        let mut limit_orders = Vec::new();
        let uniswaps = extract_deepest_amm_liquidity(&liquidity);
        for liquidity in liquidity {
            match liquidity {
                Liquidity::Limit(order) => limit_orders.push(order),
                Liquidity::Amm(_) => continue,
            }
        }
        Ok(settle(limit_orders.into_iter(), uniswaps).await)
    }
}

impl fmt::Display for NaiveSolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NaiveSolver")
    }
}

async fn settle(
    orders: impl Iterator<Item = LimitOrder>,
    uniswaps: HashMap<TokenPair, AmmOrder>,
) -> Option<Settlement> {
    let orders = organize_orders_by_token_pair(orders);
    // TODO: Settle multiple token pairs in one settlement.
    for (pair, orders) in orders {
        if let Some(settlement) = settle_pair(pair, orders, &uniswaps).await {
            return Some(settlement);
        }
    }
    None
}

async fn settle_pair(
    pair: TokenPair,
    orders: Vec<LimitOrder>,
    uniswaps: &HashMap<TokenPair, AmmOrder>,
) -> Option<Settlement> {
    let uniswap = match uniswaps.get(&pair) {
        Some(uniswap) => uniswap,
        None => {
            tracing::warn!("No AMM for: {:?}", pair);
            return None;
        }
    };
    multi_order_solver::solve(orders.into_iter(), &uniswap)
}

fn organize_orders_by_token_pair(
    orders: impl Iterator<Item = LimitOrder>,
) -> HashMap<TokenPair, Vec<LimitOrder>> {
    let mut result = HashMap::<_, Vec<LimitOrder>>::new();
    for (order, token_pair) in orders.filter(usable_order).filter_map(|order| {
        let pair = TokenPair::new(order.buy_token, order.sell_token)?;
        Some((order, pair))
    }) {
        result.entry(token_pair).or_default().push(order);
    }
    result
}

fn usable_order(order: &LimitOrder) -> bool {
    !order.sell_amount.is_zero() && !order.buy_amount.is_zero()
}
