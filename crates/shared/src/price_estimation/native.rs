use super::{NativePriceEstimating, PriceEstimating, PriceEstimationError, Query};
use model::order::OrderKind;
use primitive_types::{H160, U256};
use std::sync::Arc;

/// Wrapper around price estimators specialized to estimate a token's price compared to the current
/// chain's native token.
pub struct NativePriceEstimator {
    inner: Arc<dyn PriceEstimating>,
    native_token: H160,
    price_estimation_amount: U256,
}

impl NativePriceEstimator {
    pub fn new(
        inner: Arc<dyn PriceEstimating>,
        native_token: H160,
        price_estimation_amount: U256,
    ) -> Self {
        Self {
            inner,
            native_token,
            price_estimation_amount,
        }
    }
}

#[async_trait::async_trait]
impl NativePriceEstimating for NativePriceEstimator {
    async fn estimate_native_prices(
        &self,
        tokens: &[H160],
    ) -> Vec<Result<f64, PriceEstimationError>> {
        let native_token_queries: Vec<_> = tokens
            .iter()
            .map(|token| Query {
                sell_token: *token,
                buy_token: self.native_token,
                in_amount: self.price_estimation_amount,
                kind: OrderKind::Buy,
            })
            .collect();

        let estimates = self.inner.estimates(&native_token_queries).await;

        estimates
            .into_iter()
            .zip(native_token_queries.iter())
            .map(|(estimate, query)| {
                estimate.map(|estimate| estimate.price_in_sell_token_f64(query))
            })
            .collect()
    }
}

