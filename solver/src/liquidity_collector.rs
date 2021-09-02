use crate::{liquidity::Liquidity, liquidity::{LimitOrder, balancer::BalancerV2Liquidity, uniswap::UniswapLikeLiquidity}, orderbook::OrderBookApi};
use anyhow::{Context, Result};
use model::order::OrderUid;
use shared::recent_block_cache::Block;
use std::collections::HashSet;

pub struct LiquidityCollector {
    pub uniswap_like_liquidity: Vec<UniswapLikeLiquidity>,
    pub orderbook_api: OrderBookApi,
    pub balancer_v2_liquidity: Option<BalancerV2Liquidity>,
}

impl LiquidityCollector {
    pub async fn get_orders(
        &self,
        inflight_trades: &HashSet<OrderUid>,
    ) -> Result<Vec<LimitOrder>> {
        let limit_orders = self
            .orderbook_api
            .get_liquidity(inflight_trades)
            .await
            .context("failed to get orderbook liquidity")?;
        tracing::info!("got {} orders: {:?}", limit_orders.len(), limit_orders);
        Ok(limit_orders)
    }

    pub async fn get_liquidity_for_orders(
        &self,
        limit_orders: &[LimitOrder],
        at_block: Block,
    ) -> Result<Vec<Liquidity>> {
        let mut amms = vec![];
        for liquidity in &self.uniswap_like_liquidity {
            amms.extend(
                liquidity
                    .get_liquidity(limit_orders, at_block)
                    .await
                    .context("failed to get UniswapLike liquidity")?
                    .into_iter()
                    .map(Liquidity::ConstantProduct),
            );
        }
        if let Some(balancer_v2_liquidity) = self.balancer_v2_liquidity.as_ref() {
            amms.extend(
                balancer_v2_liquidity
                    .get_liquidity(limit_orders, at_block)
                    .await
                    .context("failed to get Balancer liquidity")?
                    .into_iter()
                    .map(Liquidity::WeightedProduct),
            );
        }
        tracing::debug!("got {} AMMs", amms.len());

        Ok(amms)
    }
}
