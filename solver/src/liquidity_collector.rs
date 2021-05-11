use anyhow::{Context, Result};

use crate::{
    liquidity::baseline_liquidity::BaselineLiquidity, liquidity::Liquidity, orderbook::OrderBookApi,
};

pub struct LiquidityCollector {
    pub baseline_liquidity: Vec<Box<dyn BaselineLiquidity>>,
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

        let mut amms = vec![];
        for liquidity in self.baseline_liquidity.iter() {
            amms.extend(
                liquidity
                    .get_liquidity(&mut limit_orders.iter())
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
