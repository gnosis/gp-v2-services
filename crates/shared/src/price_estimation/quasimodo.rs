use crate::bad_token::BadTokenDetecting;
use crate::baseline_solver::BaseTokens;
use crate::http_solver_api::constants::GAS_PER_ORDER;
use crate::http_solver_api::constants::GAS_PER_UNISWAP;
use crate::http_solver_api::model::{
    AmmModel, AmmParameters, BatchAuctionModel, ConstantProductPoolParameters, CostModel, FeeModel,
    OrderModel, TokenInfoModel,
};
use crate::http_solver_api::HttpSolverApi;
use crate::price_estimation::{
    ensure_token_supported, Estimate, PriceEstimating, PriceEstimationError, Query,
};
use crate::recent_block_cache::Block;
use crate::sources::uniswap::pool_cache::PoolCache;
use crate::sources::uniswap::pool_fetching::PoolFetching;
use crate::token_info::TokenInfoFetching;
use ethcontract::{H160, U256};
use gas_estimation::GasPriceEstimating;
use model::order::OrderKind;
use model::TokenPair;
use num::{BigInt, BigRational};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct QuasimodoPriceEstimator {
    pub api: Arc<HttpSolverApi>,
    pub pools: Arc<PoolCache>,
    pub bad_token_detector: Arc<dyn BadTokenDetecting>,
    pub token_info: Arc<dyn TokenInfoFetching>,
    pub gas_info: Arc<dyn GasPriceEstimating>,
    pub native_token: H160,
    pub base_tokens: Arc<BaseTokens>,
}

impl QuasimodoPriceEstimator {
    async fn estimate(&self, query: &Query) -> Result<Estimate, PriceEstimationError> {
        if query.buy_token == query.sell_token {
            return Ok(Estimate {
                out_amount: query.in_amount,
                gas: 0.into(),
            });
        }

        ensure_token_supported(query.buy_token, self.bad_token_detector.as_ref()).await?;
        ensure_token_supported(query.sell_token, self.bad_token_detector.as_ref()).await?;

        let gas_cost = U256::from_f64_lossy(self.gas_info.estimate().await?.legacy);

        let mut tokens = self.base_tokens.tokens().clone();
        tokens.insert(query.sell_token);
        tokens.insert(query.buy_token);
        tokens.insert(self.native_token);
        let tokens: Vec<_> = tokens.drain().collect();

        let token_infos = self.token_info.get_token_infos(&tokens).await;

        let tokens = self
            .base_tokens
            .tokens()
            .iter()
            .map(|token| {
                (
                    *token,
                    TokenInfoModel {
                        decimals: token_infos[token].decimals,
                        normalize_priority: Some(if self.native_token == query.buy_token {
                            1
                        } else {
                            0
                        }),
                        ..Default::default()
                    },
                )
            })
            .collect();

        let (sell_amount, buy_amount) = match query.kind {
            OrderKind::Buy => (U256::max_value(), query.in_amount),
            OrderKind::Sell => (query.in_amount, U256::one()),
        };

        let orders = BTreeMap::from([(
            0,
            OrderModel {
                sell_token: query.sell_token,
                buy_token: query.buy_token,
                sell_amount,
                buy_amount,
                allow_partial_fill: false,
                is_sell_order: query.kind == OrderKind::Sell,
                fee: FeeModel {
                    amount: *GAS_PER_ORDER * gas_cost,
                    token: self.native_token,
                },
                cost: CostModel {
                    amount: *GAS_PER_ORDER * gas_cost,
                    token: self.native_token,
                },
                is_liquidity_order: false,
            },
        )]);

        let token_pair = TokenPair::new(query.sell_token, query.buy_token).unwrap();
        let mut pairs = self.base_tokens.relevant_pairs([token_pair].into_iter());
        pairs.insert(token_pair);

        let amms = self
            .pools
            .fetch(pairs, Block::Recent)
            .await?
            .iter()
            .map(|pool| AmmModel {
                parameters: AmmParameters::ConstantProduct(ConstantProductPoolParameters {
                    reserves: BTreeMap::from([
                        (pool.tokens.get().0, pool.reserves.0.into()),
                        (pool.tokens.get().1, pool.reserves.1.into()),
                    ]),
                }),
                fee: BigRational::from((
                    BigInt::from(*pool.fee.numer()),
                    BigInt::from(*pool.fee.denom()),
                )),
                cost: CostModel {
                    amount: *GAS_PER_UNISWAP * gas_cost,
                    token: self.native_token,
                },
                mandatory: false,
            })
            .enumerate()
            .collect();

        let settlement = self
            .api
            .solve(
                &BatchAuctionModel {
                    tokens,
                    orders,
                    amms,
                    metadata: None,
                },
                Instant::now() + Duration::from_secs(5),
            )
            .await?;

        if settlement.orders.is_empty() {
            return Err(PriceEstimationError::NoLiquidity);
        }

        let mut cost = self.extract_cost(&settlement.orders[&0].cost)?;
        for amm in settlement.amms.values() {
            cost += self.extract_cost(&amm.cost)? * amm.execution.len();
        }

        Ok(Estimate {
            out_amount: match query.kind {
                OrderKind::Buy => settlement.orders[&0].exec_sell_amount,
                OrderKind::Sell => settlement.orders[&0].exec_buy_amount,
            },
            gas: cost / gas_cost,
        })
    }

    fn extract_cost(&self, cost: &Option<CostModel>) -> Result<U256, PriceEstimationError> {
        if let Some(cost) = cost {
            if cost.token != self.native_token {
                Err(anyhow::anyhow!("cost specified as an unknown token {}", cost.token).into())
            } else {
                Ok(cost.amount)
            }
        } else {
            Ok(U256::zero())
        }
    }
}

#[async_trait::async_trait]
impl PriceEstimating for QuasimodoPriceEstimator {
    async fn estimates(
        &self,
        queries: &[Query],
    ) -> Vec<anyhow::Result<Estimate, PriceEstimationError>> {
        let mut results = Vec::with_capacity(queries.len());

        for query in queries {
            results.push(self.estimate(query).await);
        }

        results
    }
}
