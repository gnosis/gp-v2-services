use super::{Estimate, PriceEstimating, PriceEstimationError, Query};
use crate::bad_token::{BadTokenDetecting, TokenQuality};
use crate::price_estimation::gas::GAS_PER_WETH_UNWRAP;
use anyhow::Result;
use model::order::BUY_ETH_ADDRESS;
use primitive_types::H160;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Verifies that buy and sell tokens are supported and handles
/// ETH as buy token appropriately.
pub struct SanitizedPriceEstimator<T: PriceEstimating> {
    inner: T,
    bad_token_detector: Arc<dyn BadTokenDetecting>,
    native_token: H160,
}

type EstimationResult = Result<Estimate, PriceEstimationError>;

enum EstimationProgress {
    TrivialSolution(EstimationResult),
    AwaitingEthEstimation,
    AwaitingErc20Estimation,
}

impl<T: PriceEstimating> SanitizedPriceEstimator<T> {
    pub fn new(
        inner: T,
        native_token: H160,
        bad_token_detector: Arc<dyn BadTokenDetecting>,
    ) -> Self {
        Self {
            inner,
            native_token,
            bad_token_detector,
        }
    }

    async fn get_token_quality_errors(
        &self,
        queries: &[Query],
    ) -> HashMap<H160, PriceEstimationError> {
        let mut token_quality_errors: HashMap<H160, PriceEstimationError> = Default::default();
        let mut checked_tokens = HashSet::<H160>::default();

        // TODO should this be parallelised?
        for token in queries
            .iter()
            .copied()
            .flat_map(|query| [query.buy_token, query.sell_token])
        {
            if checked_tokens.contains(&token) {
                continue;
            }

            match self.bad_token_detector.detect(token).await {
                Err(err) => {
                    token_quality_errors.insert(token, PriceEstimationError::Other(err));
                }
                Ok(TokenQuality::Bad { .. }) => {
                    token_quality_errors
                        .insert(token, PriceEstimationError::UnsupportedToken(token));
                }
                _ => (),
            };
            checked_tokens.insert(token);
        }
        token_quality_errors
    }

    async fn bulk_estimate_prices(
        &self,
        queries: &[Query],
    ) -> (Vec<EstimationProgress>, Vec<EstimationResult>) {
        let token_quality_errors = self.get_token_quality_errors(queries).await;

        let mut estimations_to_forward = Vec::new();

        let estimation_progress = queries
            .iter()
            .map(|query| {
                if let Some(err) = token_quality_errors.get(&query.buy_token) {
                    return EstimationProgress::TrivialSolution(Err(err.clone()));
                }
                if let Some(err) = token_quality_errors.get(&query.sell_token) {
                    return EstimationProgress::TrivialSolution(Err(err.clone()));
                }

                if query.buy_token == query.sell_token {
                    return EstimationProgress::TrivialSolution(Ok(Estimate {
                        out_amount: query.in_amount,
                        gas: 0.into(),
                    }));
                }

                if query.buy_token == BUY_ETH_ADDRESS {
                    estimations_to_forward.push(Query {
                        buy_token: self.native_token,
                        ..*query
                    });
                    return EstimationProgress::AwaitingEthEstimation;
                }

                estimations_to_forward.push(*query);
                EstimationProgress::AwaitingErc20Estimation
            })
            .collect::<Vec<_>>();

        let remaining_estimations = self.inner.estimates(&estimations_to_forward[..]).await;
        (estimation_progress, remaining_estimations)
    }

    fn merge_trivial_and_forwarded_estimations(
        &self,
        partially_estimated: Vec<EstimationProgress>,
        forwarded_estimations: Vec<EstimationResult>,
    ) -> Vec<EstimationResult> {
        use EstimationProgress::*;

        let mut forwarded_estimations = forwarded_estimations.into_iter();

        let merged_results =
            partially_estimated
                .into_iter()
                .map(|progress| match progress {
                    TrivialSolution(res) => res,
                    AwaitingErc20Estimation => forwarded_estimations
                        .next()
                        .expect("there is a result for every forwarded estimation"),
                    AwaitingEthEstimation => {
                        let mut res = forwarded_estimations
                            .next()
                            .expect("there is a result for every forwarded estimation")?;
                        res.gas = res.gas.checked_add(GAS_PER_WETH_UNWRAP.into()).ok_or(
                            anyhow::anyhow!("cost of unwrapping ETH would overflow gas price"),
                        )?;
                        Ok(res)
                    }
                })
                .collect();
        debug_assert!(forwarded_estimations.next().is_none());
        merged_results
    }
}

#[async_trait::async_trait]
impl<T: PriceEstimating> PriceEstimating for SanitizedPriceEstimator<T> {
    async fn estimates(&self, queries: &[Query]) -> Vec<EstimationResult> {
        let (trivial_estimations, forwarded_estimations) = self.bulk_estimate_prices(queries).await;
        self.merge_trivial_and_forwarded_estimations(trivial_estimations, forwarded_estimations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bad_token::{MockBadTokenDetecting, TokenQuality};
    use crate::price_estimation::MockPriceEstimating;
    use model::order::OrderKind;
    use primitive_types::{H160, U256};

    const BAD_TOKEN: H160 = H160([0x12; 20]);

    #[tokio::test]
    async fn handles_trivial_estimates_on_its_own() {
        let mut bad_token_detector = MockBadTokenDetecting::new();
        bad_token_detector.expect_detect().returning(|token| {
            if token == BAD_TOKEN {
                Ok(TokenQuality::Bad {
                    reason: "Token not supported".into(),
                })
            } else {
                Ok(TokenQuality::Good)
            }
        });

        let native_token = H160::from_low_u64_le(1);

        let queries = [
            // This is the common case (Tokens are supported, distinct and not ETH).
            // Will be estimated by the wrapped_estimator.
            Query {
                sell_token: H160::from_low_u64_le(1),
                buy_token: H160::from_low_u64_le(2),
                in_amount: 1.into(),
                kind: OrderKind::Buy,
            },
            // `sanitized_estimator` will replace `buy_token` with `native_token` before querying
            // `wrapped_estimator`.
            // `sanitized_estimator` will add cost of unwrapping ETH to Estimate.
            Query {
                sell_token: H160::from_low_u64_le(1),
                buy_token: BUY_ETH_ADDRESS,
                in_amount: 1.into(),
                kind: OrderKind::Buy,
            },
            // Will cause buffer overflow of gas price in `sanitized_estimator`.
            Query {
                sell_token: H160::from_low_u64_le(1),
                buy_token: BUY_ETH_ADDRESS,
                in_amount: U256::MAX,
                kind: OrderKind::Buy,
            },
            // Can be estimated by `sanitized_estimator` because `buy_token` and `sell_token` are identical.
            Query {
                sell_token: H160::from_low_u64_le(1),
                buy_token: H160::from_low_u64_le(1),
                in_amount: 1.into(),
                kind: OrderKind::Sell,
            },
            // Will throw `UnsupportedToken` error in `sanitized_estimator`.
            Query {
                sell_token: BAD_TOKEN,
                buy_token: H160::from_low_u64_le(1),
                in_amount: 1.into(),
                kind: OrderKind::Buy,
            },
            // Will throw `UnsupportedToken` error in `sanitized_estimator`.
            Query {
                sell_token: H160::from_low_u64_le(1),
                buy_token: BAD_TOKEN,
                in_amount: 1.into(),
                kind: OrderKind::Buy,
            },
        ];

        let expected_forwarded_queries = [
            // SanitizedPriceEstimator will simply forward the Query in the common case
            queries[0],
            Query {
                // SanitizedPriceEstimator replaces ETH buy token with native token
                buy_token: native_token,
                ..queries[1]
            },
            Query {
                // SanitizedPriceEstimator replaces ETH buy token with native token
                buy_token: native_token,
                ..queries[2]
            },
        ];

        let mut wrapped_estimator = MockPriceEstimating::new();
        wrapped_estimator
            .expect_estimates()
            .times(1)
            .withf(move |arg: &[Query]| arg.iter().eq(expected_forwarded_queries.iter()))
            .returning(|_| {
                vec![
                    Ok(Estimate {
                        out_amount: 1.into(),
                        gas: 100.into(),
                    }),
                    Ok(Estimate {
                        out_amount: 1.into(),
                        gas: 100.into(),
                    }),
                    Ok(Estimate {
                        out_amount: 1.into(),
                        gas: U256::MAX,
                    }),
                ]
            });

        let sanitized_estimator = SanitizedPriceEstimator {
            inner: wrapped_estimator,
            bad_token_detector: Arc::new(bad_token_detector),
            native_token,
        };

        let result = sanitized_estimator.estimates(&queries).await;
        assert_eq!(result.len(), 6);
        assert_eq!(
            result[0].as_ref().unwrap(),
            &Estimate {
                out_amount: 1.into(),
                gas: 100.into()
            }
        );
        assert_eq!(
            result[1].as_ref().unwrap(),
            &Estimate {
                out_amount: 1.into(),
                //sanitized_estimator will add ETH_UNWRAP_COST to the gas of any
                //Query with ETH as the buy_token.
                gas: U256::from(GAS_PER_WETH_UNWRAP)
                    .checked_add(100.into())
                    .unwrap()
            }
        );
        assert!(matches!(
            result[2].as_ref().unwrap_err(),
            PriceEstimationError::Other(err)
                if err.to_string() == "cost of unwrapping ETH would overflow gas price",
        ));
        assert_eq!(
            result[3].as_ref().unwrap(),
            &Estimate {
                out_amount: 1.into(),
                gas: 0.into()
            }
        );
        assert!(matches!(
            result[4].as_ref().unwrap_err(),
            PriceEstimationError::UnsupportedToken(_)
        ));
        assert!(matches!(
            result[5].as_ref().unwrap_err(),
            PriceEstimationError::UnsupportedToken(_)
        ));
    }
}
