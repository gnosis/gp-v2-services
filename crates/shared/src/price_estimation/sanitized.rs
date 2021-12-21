use super::{ensure_token_supported, Estimate, PriceEstimating, PriceEstimationError, Query};
use crate::bad_token::BadTokenDetecting;
use crate::price_estimation::gas::GAS_PER_WETH_UNWRAP;
use anyhow::Result;
use futures::future;
use model::order::BUY_ETH_ADDRESS;
use primitive_types::H160;
use std::sync::Arc;

/// Verifies that buy and sell tokens are supported and handles
/// ETH as buy token appropriately.
pub struct SanitizedPriceEstimator<T: PriceEstimating> {
    inner: T,
    bad_token_detector: Arc<dyn BadTokenDetecting>,
    native_token: H160,
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
    async fn estimate_sanitized_query(
        &self,
        query: &Query,
    ) -> Result<Estimate, PriceEstimationError> {
        if query.buy_token == query.sell_token {
            return Ok(Estimate {
                out_amount: query.in_amount,
                gas: 0.into(),
            });
        }

        ensure_token_supported(query.buy_token, self.bad_token_detector.as_ref()).await?;
        ensure_token_supported(query.sell_token, self.bad_token_detector.as_ref()).await?;

        let buy_eth = query.buy_token == BUY_ETH_ADDRESS;
        let sanitized_query = if buy_eth {
            Query {
                buy_token: self.native_token,
                ..*query
            }
        } else {
            *query
        };

        let mut estimated_price = self.inner.estimate(&sanitized_query).await?;

        if buy_eth {
            estimated_price.gas = estimated_price
                .gas
                .checked_add(GAS_PER_WETH_UNWRAP.into())
                .ok_or(anyhow::anyhow!(
                    "cost of unwrapping ETH would overflow gas price"
                ))?;
        }

        Ok(estimated_price)
    }
}

#[async_trait::async_trait]
impl<T: PriceEstimating> PriceEstimating for SanitizedPriceEstimator<T> {
    async fn estimates(&self, queries: &[Query]) -> Vec<Result<Estimate, PriceEstimationError>> {
        future::join_all(
            queries
                .iter()
                .map(|query| self.estimate_sanitized_query(query)),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bad_token::{MockBadTokenDetecting, TokenQuality};
    use crate::price_estimation::MockPriceEstimating;
    use mockall::predicate::eq;
    use model::order::OrderKind;
    use primitive_types::{H160, U256};

    const BAD_TOKEN: H160 = H160([0x12; 20]);

    #[tokio::test]
    async fn works() {
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

        let mut wrapped_estimator = MockPriceEstimating::new();
        // Tests for estimates[0]
        wrapped_estimator
            .expect_estimate()
            .times(1)
            .with(eq(queries[0]))
            .returning(|_| {
                Ok(Estimate {
                    out_amount: 1.into(),
                    gas: 100.into(),
                })
            });
        // Tests for estimates[1]
        wrapped_estimator
            .expect_estimate()
            .times(1)
            .with(eq(Query {
                // SanitizedPriceEstimator replaces ETH buy token with native token
                buy_token: native_token,
                ..queries[1]
            }))
            .returning(|_| {
                Ok(Estimate {
                    out_amount: 1.into(),
                    gas: 100.into(),
                })
            });
        // Tests for estimates[2]
        wrapped_estimator
            .expect_estimate()
            .times(1)
            .with(eq(Query {
                // SanitizedPriceEstimator replaces ETH buy token with native token
                buy_token: native_token,
                ..queries[2]
            }))
            .returning(|_| {
                Ok(Estimate {
                    out_amount: 1.into(),
                    gas: U256::MAX,
                })
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
