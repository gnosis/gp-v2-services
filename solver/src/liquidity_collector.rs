use anyhow::{Context, Result};
use ethcontract::BlockNumber;

use crate::{
    liquidity::uniswap::UniswapLikeLiquidity, liquidity::Liquidity, orderbook::OrderBookApi,
};

pub struct LiquidityCollector {
    pub uniswap_like_liquidity: Vec<UniswapLikeLiquidity>,
    pub orderbook_api: OrderBookApi,
}

impl LiquidityCollector {
    pub async fn get_liquidity(&self, at_block: BlockNumber) -> Result<Vec<Liquidity>> {
        let limit_orders = self
            .orderbook_api
            .get_liquidity()
            .await
            .context("failed to get orderbook")?;
        tracing::debug!("got {} orders", limit_orders.len());

        let mut amms = vec![];
        for liquidity in self.uniswap_like_liquidity.iter() {
            amms.extend(
                liquidity
                    .get_liquidity(limit_orders.iter(), at_block)
                    .await
                    .context("failed to get pool")?,
            );
        }
        tracing::debug!("got {} AMMs", amms.len());

        Ok(limit_orders
            .into_iter()
            .map(Liquidity::Limit)
            .chain(amms.into_iter().map(Liquidity::Amm))
            .collect())
    }
}
