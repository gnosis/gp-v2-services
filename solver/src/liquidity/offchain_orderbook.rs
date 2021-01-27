use crate::orderbook::OrderBookApi;
use crate::settlement::{Interaction, Trade};
use anyhow::{Context, Result};
use model::order::OrderCreation;
use primitive_types::U256;
use std::sync::Arc;

use super::{LimitOrder, LimitOrderSettlementHandling, Liquidity, LiquiditySource};

#[async_trait::async_trait]
impl LiquiditySource for OrderBookApi {
    async fn get_liquidity(
        &self,
        _: impl Iterator<Item = &Liquidity> + Send + 'async_trait,
    ) -> Result<Vec<Liquidity>> {
        Ok(self
            .get_orders()
            .await
            .context("failed to get orderbook")?
            .into_iter()
            .map(|order| Liquidity::Limit(order.order_creation.into()))
            .collect())
    }
}

impl Into<LimitOrder> for OrderCreation {
    fn into(self) -> LimitOrder {
        LimitOrder {
            sell_token: self.sell_token,
            // TODO handle ETH buy token address (0xe...e) by making the handler include an WETH.unwrap() interaction
            buy_token: self.buy_token,
            // TODO discount previously executed sell amount
            sell_amount: self.sell_amount,
            buy_amount: self.buy_amount,
            kind: self.kind,
            partially_fillable: self.partially_fillable,
            settlement_handling: Arc::new(self),
        }
    }
}

impl LimitOrderSettlementHandling for OrderCreation {
    fn settle(&self, executed_amount: U256) -> (Option<Trade>, Vec<Box<dyn Interaction>>) {
        (Some(Trade::matched(*self, executed_amount)), Vec::new())
    }
}
