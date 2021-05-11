use anyhow::{Context, Result};

use crate::liquidity::LimitOrder;
use crate::{
    liquidity::baseline_liquidity::BaselineLiquidity, liquidity::Liquidity, orderbook::OrderBookApi,
};
use ethcontract::H160;
use model::TokenPair;
use shared::baseline_solver::{path_candidates, token_path_to_pair_path, MAX_HOPS};
use std::collections::HashSet;

pub struct LiquidityCollector {
    pub baseline_liquidity: Vec<Box<dyn BaselineLiquidity>>,
    pub base_tokens: HashSet<H160>,
    pub orderbook_api: OrderBookApi,
}

impl LiquidityCollector {
    pub async fn get_liquidity(&self) -> Result<Vec<Liquidity>> {
        let limit_orders = self
            .orderbook_api
            .get_liquidity()
            .await
            .context("failed to get orderbook")?;
        tracing::debug!("got {} orders", limit_orders.len());

        let pools = self.get_pools(&limit_orders);
        let mut amms = vec![];
        for liquidity in self.baseline_liquidity.iter() {
            amms.extend(
                liquidity
                    .get_liquidity(pools.clone())
                    .await
                    .context("failed to get pool")?,
            );
        }
        tracing::debug!("got {} AMMs", amms.len());

        Ok(limit_orders
            .into_iter()
            .map(Liquidity::Limit)
            .chain(amms)
            .collect())
    }

    fn get_pools<'a>(
        &self,
        offchain_orders: impl IntoIterator<Item = &'a LimitOrder> + 'a,
    ) -> HashSet<TokenPair> {
        let mut pools = HashSet::new();

        for order in offchain_orders {
            let path_candidates = path_candidates(
                order.sell_token,
                order.buy_token,
                &self.base_tokens,
                MAX_HOPS,
            );
            pools.extend(
                path_candidates
                    .iter()
                    .flat_map(|candidate| token_path_to_pair_path(&candidate).into_iter()),
            );
        }
        pools
    }
}
