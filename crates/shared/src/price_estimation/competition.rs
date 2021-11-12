use super::{Estimate, PriceEstimating, PriceEstimationError, Query};
use anyhow::{anyhow, Result};
use futures::future;

/// Price estimator that pulls estimates from various sources
/// and competes on the best price.
pub struct CompetitionPriceEstimator {
    inner: Vec<(String, Box<dyn PriceEstimating>)>,
}

impl CompetitionPriceEstimator {
    pub fn new(inner: Vec<(String, Box<dyn PriceEstimating>)>) -> Self {
        assert!(!inner.is_empty());
        Self { inner }
    }
}

#[async_trait::async_trait]
impl PriceEstimating for CompetitionPriceEstimator {
    async fn estimates(&self, queries: &[Query]) -> Vec<Result<Estimate, PriceEstimationError>> {
        let all_estimates =
            future::join_all(self.inner.iter().map(|(name, estimator)| async move {
                (name, estimator.estimates(queries).await)
            }))
            .await;

        queries
            .iter()
            .enumerate()
            .map(|(i, query)| {
                all_estimates
                    .iter()
                    .filter_map(|(name, estimates)| match &estimates[i] {
                        Ok(estimate) => Some((name, estimate)),
                        Err(err) => {
                            tracing::warn!(
                                estimator_name = %name, ?query, ?err,
                                "price estimation error",
                            );
                            None
                        }
                    })
                    .filter_map(|(name, estimate)| {
                        match estimate.price_in_sell_token_rational(&query) {
                            Some(price) => Some((name, estimate, price)),
                            None => {
                                tracing::warn!(
                                    estimator_name = %name, ?query, ?estimate,
                                    "price estimate with zero amounts",
                                );
                                None
                            }
                        }
                    })
                    .max_by_key(|(_, _, price)| price.clone())
                    .ok_or_else(|| {
                        PriceEstimationError::Other(anyhow!("no successful price estimates"))
                    })
                    .map(|(name, estimate, _)| {
                        tracing::debug!(
                            winning_estimator = %name, ?query, ?estimate,
                            "winning price estimate",
                        );
                        estimate.clone()
                    })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::price_estimation::MockPriceEstimating;
    use anyhow::anyhow;
    use model::order::OrderKind;
    use primitive_types::H160;

    #[tokio::test]
    async fn works() {
        let queries = [
            Query {
                sell_token: H160::from_low_u64_le(0),
                buy_token: H160::from_low_u64_le(1),
                in_amount: 1.into(),
                kind: OrderKind::Buy,
            },
            Query {
                sell_token: H160::from_low_u64_le(2),
                buy_token: H160::from_low_u64_le(3),
                in_amount: 1.into(),
                kind: OrderKind::Buy,
            },
            Query {
                sell_token: H160::from_low_u64_le(3),
                buy_token: H160::from_low_u64_le(4),
                in_amount: 1.into(),
                kind: OrderKind::Buy,
            },
        ];
        let estimates = [
            Estimate {
                out_amount: 1.into(),
                ..Default::default()
            },
            Estimate {
                out_amount: 2.into(),
                ..Default::default()
            },
        ];

        let mut first = MockPriceEstimating::new();
        first.expect_estimates().times(1).returning({
            let estimates = estimates.clone();
            move |queries| {
                assert_eq!(queries.len(), 3);
                vec![
                    Ok(estimates[0].clone()),
                    Ok(estimates[0].clone()),
                    Err(PriceEstimationError::Other(anyhow!(""))),
                ]
            }
        });
        let mut second = MockPriceEstimating::new();
        second.expect_estimates().times(1).returning({
            let estimates = estimates.clone();
            move |queries| {
                assert_eq!(queries.len(), 3);
                vec![
                    Err(PriceEstimationError::Other(anyhow!(""))),
                    Ok(estimates[1].clone()),
                    Err(PriceEstimationError::Other(anyhow!(""))),
                ]
            }
        });

        let priority = CompetitionPriceEstimator::new(vec![
            ("first".to_owned(), Box::new(first)),
            ("second".to_owned(), Box::new(second)),
        ]);

        let result = priority.estimates(&queries).await;
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].as_ref().unwrap(), &estimates[0]);
        assert_eq!(result[1].as_ref().unwrap(), &estimates[1]);
        assert!(matches!(
            result[2].as_ref().unwrap_err(),
            PriceEstimationError::Other(_),
        ));
    }
}
