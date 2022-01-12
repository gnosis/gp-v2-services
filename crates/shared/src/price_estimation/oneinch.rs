use super::gas;
use crate::oneinch_api::{OneInchClient, RestResponse, SellOrderQuoteQuery};
use crate::price_estimation::{Estimate, PriceEstimating, PriceEstimationError, Query};
use futures::future;
use model::order::OrderKind;
use primitive_types::U256;
use std::sync::Arc;

pub struct OneInchPriceEstimator {
    pub api: Arc<dyn OneInchClient>,
}

impl OneInchPriceEstimator {
    async fn estimate(&self, query: &Query) -> Result<Estimate, PriceEstimationError> {
        if query.kind == OrderKind::Buy {
            return Err(PriceEstimationError::UnsupportedOrderType);
        }

        let quote = self
            .api
            .get_sell_order_quote(SellOrderQuoteQuery {
                from_token_address: query.sell_token,
                to_token_address: query.buy_token,
                amount: query.in_amount,
                protocols: None,
                fee: None,
                gas_limit: None,
                connector_tokens: None,
                complexity_level: None,
                main_route_parts: None,
                virtual_parts: None,
                parts: None,
                gas_price: None,
            })
            .await
            .map_err(PriceEstimationError::Other)?;

        match quote {
            RestResponse::Ok(quote) => Ok(Estimate {
                out_amount: quote.to_token_amount,
                gas: U256::from(gas::SETTLEMENT_OVERHEAD) + quote.estimated_gas,
            }),
            RestResponse::Err(e) => {
                Err(PriceEstimationError::Other(anyhow::anyhow!(e.description)))
            }
        }
    }
}

#[async_trait::async_trait]
impl PriceEstimating for OneInchPriceEstimator {
    async fn estimates(
        &self,
        queries: &[Query],
    ) -> Vec<anyhow::Result<Estimate, PriceEstimationError>> {
        debug_assert!(queries.iter().all(|query| {
            query.buy_token != model::order::BUY_ETH_ADDRESS
                && query.sell_token != model::order::BUY_ETH_ADDRESS
                && query.sell_token != query.buy_token
        }));

        future::join_all(
            queries
                .iter()
                .map(|query| async move { self.estimate(query).await }),
        )
        .await
    }
}

