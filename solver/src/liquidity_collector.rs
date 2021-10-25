use crate::{
    liquidity::Liquidity,
    liquidity::{balancer::BalancerV2Liquidity, uniswap::UniswapLikeLiquidity, LimitOrder},
};
use anyhow::{Context, Result};
use shared::recent_block_cache::Block;

pub struct LiquidityCollector {
    pub uniswap_like_liquidity: Vec<UniswapLikeLiquidity>,
    pub balancer_v2_liquidity: Option<BalancerV2Liquidity>,
}

impl LiquidityCollector {
    pub async fn get_liquidity_for_orders(
        &self,
        limit_orders: &[LimitOrder],
        at_block: Block,
    ) -> Result<Vec<Liquidity>> {
        let mut amms = vec![];
        let (user_orders, pmm_orders): (Vec<_>, Vec<_>) = limit_orders
            .to_vec()
            .into_iter()
            .partition(|order| !order.is_liquidity_order);
        amms.extend(pmm_orders.into_iter().map(Liquidity::PrivateMarketMaker));
        for liquidity in &self.uniswap_like_liquidity {
            amms.extend(
                liquidity
                    .get_liquidity(user_orders.as_slice(), at_block)
                    .await
                    .context("failed to get UniswapLike liquidity")?
                    .into_iter()
                    .map(Liquidity::ConstantProduct),
            );
        }
        if let Some(balancer_v2_liquidity) = self.balancer_v2_liquidity.as_ref() {
            let (stable_orders, weighted_orders) = balancer_v2_liquidity
                .get_liquidity(user_orders.as_slice(), at_block)
                .await
                .context("failed to get Balancer liquidity")?;

            amms.extend(weighted_orders.into_iter().map(Liquidity::BalancerWeighted));
            amms.extend(stable_orders.into_iter().map(Liquidity::BalancerStable));
        }
        tracing::debug!("got {} AMMs", amms.len());

        Ok(amms)
    }
}
