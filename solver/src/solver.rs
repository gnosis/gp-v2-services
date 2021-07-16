//! Provides access to different solving strategies over an abstract interface.

use crate::liquidity::Liquidity;
use crate::metrics::SolverMetrics;
use crate::settlement::Settlement;
use anyhow::{anyhow, Result};
use baseline_solver::BaselineSolver;
use contracts::GPv2Settlement;
use ethcontract::{Account, H160, U256};
use futures::future::join_all;
use http_solver::{HttpSolver, SolverConfig};
use matcha_solver::MatchaSolver;
use naive_solver::NaiveSolver;
use oneinch_solver::OneInchSolver;
use paraswap_solver::ParaswapSolver;
use reqwest::Url;
use shared::conversions::U256Ext;
use shared::price_estimate::PriceEstimating;
use shared::token_info::TokenInfoFetching;
use shared::Web3;
use single_order_solver::SingleOrderSolver;
use std::collections::{HashMap, HashSet};
use std::fmt::Formatter;
use std::sync::Arc;
use std::time::{Duration, Instant};
use structopt::clap::arg_enum;

mod baseline_solver;
mod http_solver;
mod matcha_solver;
mod naive_solver;
mod oneinch_solver;
mod paraswap_solver;
mod single_order_solver;
mod solver_utils;

// For solvers that enforce a timeout internally we set their timeout to the global solver timeout
// minus this duration to account for additional delay for example from the network.
const TIMEOUT_SAFETY_BUFFER: Duration = Duration::from_secs(5);

/// Interface that all solvers must implement.
///
/// A `solve` method transforming a collection of `Liquidity` (sources) into a list of
/// independent `Settlements`. Solvers are free to choose which types `Liquidity` they
/// would like to process, including their own private sources.
#[async_trait::async_trait]
pub trait Solver {
    /// The returned settlements should be independent (for example not reusing the same user
    /// order) so that they can be merged by the driver at its leisure.
    async fn solve(&self, orders: Vec<Liquidity>, gas_price: f64) -> Result<Vec<Settlement>>;

    /// Returns solver's account that should be used to submit settlements.
    fn account(&self) -> &Account;

    /// Returns displayable name of the solver.
    ///
    /// This method is used in logging and metrics. By default, returns the type name.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

/// Solver ID can be used to identify a solver within a [`Solvers`] collection.
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct SolverId(usize);

impl SolverId {
    /// Create an object that can be formatted to display
    /// solver ID's numerical value, name and address.
    ///
    /// Prefer this function over [`debug_raw`] if you have access
    /// to the solvers collection.
    pub fn debug(self, solvers: &Solvers) -> impl std::fmt::Debug + '_ {
        struct SolverIdDebug<'a>(usize, &'a str, &'a Account);
        impl std::fmt::Debug for SolverIdDebug<'_> {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.debug_struct("SolverId")
                    .field("id", &self.0)
                    .field("name", &self.1)
                    .field("address", &self.2.address())
                    .finish()
            }
        }

        SolverIdDebug(self.0, solvers.name(self), solvers.account(self))
    }

    /// Create an object that can be formatted to display
    /// solver ID's numerical value.
    pub fn debug_raw(self) -> impl std::fmt::Debug {
        struct SolverIdDebug(usize);
        impl std::fmt::Debug for SolverIdDebug {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.debug_tuple("SolverId").field(&self.0).finish()
            }
        }

        SolverIdDebug(self.0)
    }

    /// Format this ID as if using [`debug_raw`].
    pub fn format(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        f.debug_tuple("SolverId").field(&self.0).finish()
    }
}

/// A collection of solvers.
pub struct Solvers {
    solvers: Vec<Box<dyn Solver>>,
}

impl Solvers {
    /// Create a new collection of solvers.
    pub fn new(solvers: Vec<Box<dyn Solver>>) -> Self {
        Solvers { solvers }
    }

    /// Returns solver by its ID.
    ///
    /// # Panics
    ///
    /// This method may panic or return an unspecified solver if given
    /// solver ID does not belong to a solver from this collection.
    pub fn solver(&self, id: SolverId) -> &dyn Solver {
        self.solvers.get(id.0).expect("invalid solver ID").as_ref()
    }

    /// Returns solver's name by its ID.
    ///
    /// See [`solver`] method for more info.
    pub fn name(&self, id: SolverId) -> &'static str {
        self.solver(id).name()
    }

    /// Returns solver's account by its ID.
    ///
    /// See [`solver`] method for more info.
    pub fn account(&self, id: SolverId) -> &Account {
        self.solver(id).account()
    }

    /// Returns an iterator over all solvers in this collection, along with their IDs.
    pub fn solvers(&self) -> impl std::iter::Iterator<Item = (SolverId, &dyn Solver)> {
        self.solvers
            .iter()
            .enumerate()
            .map(|(i, s)| (SolverId(i), s.as_ref()))
    }

    /// Runs all solvers and returns whatever settlements they've produced.
    ///
    /// Optionally reports per-solver metrics.
    pub async fn run(
        &self,
        liquidity: Vec<Liquidity>,
        gas_price: f64,
        time_limit: Duration,
        metrics: Option<&dyn SolverMetrics>,
    ) -> impl Iterator<Item = (SolverId, Result<Vec<Settlement>>)> {
        join_all(self.solvers().map(|(id, solver)| {
            let liquidity = liquidity.clone();
            async move {
                let start_time = Instant::now();

                let result = match tokio::time::timeout(
                    time_limit,
                    solver.solve(liquidity, gas_price),
                )
                .await
                {
                    Ok(inner) => inner,
                    Err(_timeout) => Err(anyhow!("solver timed out")),
                };

                if let Some(metrics) = metrics {
                    metrics.settlement_computed(solver.name(), start_time);
                }
                (id, result)
            }
        }))
        .await
        .into_iter()
    }
}

arg_enum! {
    #[derive(Debug)]
    pub enum SolverType {
        Naive,
        Baseline,
        Mip,
        OneInch,
        Paraswap,
        Matcha,
        Quasimodo,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn create(
    account: Account,
    web3: Web3,
    solvers: Vec<SolverType>,
    base_tokens: HashSet<H160>,
    native_token: H160,
    mip_solver_url: Url,
    quasimodo_solver_url: Url,
    settlement_contract: &GPv2Settlement,
    token_info_fetcher: Arc<dyn TokenInfoFetching>,
    price_estimator: Arc<dyn PriceEstimating>,
    network_id: String,
    chain_id: u64,
    fee_discount_factor: f64,
    solver_timeout: Duration,
    min_order_size_one_inch: U256,
    disabled_one_inch_protocols: Vec<String>,
    paraswap_slippage_bps: usize,
) -> Result<Solvers> {
    // Tiny helper function to help out with type inference. Otherwise, all
    // `Box::new(...)` expressions would have to be cast `as Box<dyn Solver>`.
    fn boxed(solver: impl Solver + 'static) -> Result<Box<dyn Solver>> {
        Ok(Box::new(solver))
    }

    let time_limit = solver_timeout
        .checked_sub(TIMEOUT_SAFETY_BUFFER)
        .expect("solver_timeout too low");

    // Helper function to create http solver instances.
    let create_http_solver = |url: Url| -> HttpSolver {
        HttpSolver::new(
            account.clone(),
            url,
            None,
            SolverConfig {
                max_nr_exec_orders: 100,
                time_limit: time_limit.as_secs() as u32,
            },
            native_token,
            token_info_fetcher.clone(),
            price_estimator.clone(),
            network_id.clone(),
            chain_id,
            fee_discount_factor,
        )
    };

    solvers
        .into_iter()
        .map(|solver_type| match solver_type {
            SolverType::Naive => boxed(NaiveSolver::new(account.clone())),
            SolverType::Baseline => {
                boxed(BaselineSolver::new(account.clone(), base_tokens.clone()))
            }
            SolverType::Mip => boxed(create_http_solver(mip_solver_url.clone())),
            SolverType::Quasimodo => boxed(create_http_solver(quasimodo_solver_url.clone())),
            SolverType::OneInch => {
                let one_inch_solver: SingleOrderSolver<_> = OneInchSolver::with_disabled_protocols(
                    account.clone(),
                    web3.clone(),
                    settlement_contract.clone(),
                    chain_id,
                    disabled_one_inch_protocols.clone(),
                )?
                .into();
                // We only want to use 1Inch for high value orders
                boxed(SellVolumeFilteringSolver::new(
                    Box::new(one_inch_solver),
                    price_estimator.clone(),
                    native_token,
                    min_order_size_one_inch,
                ))
            }
            SolverType::Matcha => {
                let matcha_solver = MatchaSolver::new(
                    account.clone(),
                    web3.clone(),
                    settlement_contract.clone(),
                    chain_id,
                )
                .unwrap();
                boxed(SingleOrderSolver::from(matcha_solver))
            }
            SolverType::Paraswap => boxed(SingleOrderSolver::from(ParaswapSolver::new(
                account.clone(),
                web3.clone(),
                settlement_contract.clone(),
                account.address(),
                token_info_fetcher.clone(),
                paraswap_slippage_bps,
            ))),
        })
        .collect::<Result<_>>()
        .map(Solvers::new)
}

/// Returns a naive solver to be used e.g. in e2e tests.
pub fn naive_solver(account: Account) -> Box<dyn Solver> {
    Box::new(NaiveSolver::new(account))
}

/// A solver that remove limit order below a certain threshold and
/// passes the remaining liquidity onto an inner solver implementation.
pub struct SellVolumeFilteringSolver {
    inner: Box<dyn Solver + Send + Sync>,
    price_estimator: Arc<dyn PriceEstimating>,
    denominator_token: H160,
    min_value: U256,
}

impl SellVolumeFilteringSolver {
    pub fn new(
        inner: Box<dyn Solver + Send + Sync>,
        price_estimator: Arc<dyn PriceEstimating>,
        denominator_token: H160,
        min_value: U256,
    ) -> Self {
        Self {
            inner,
            price_estimator,
            denominator_token,
            min_value,
        }
    }

    async fn filter_liquidity(&self, orders: Vec<Liquidity>) -> Vec<Liquidity> {
        let sell_tokens: Vec<_> = orders
            .iter()
            .filter_map(|order| {
                if let Liquidity::Limit(order) = order {
                    Some(order.sell_token)
                } else {
                    None
                }
            })
            .collect();
        let prices: HashMap<_, _> = self
            .price_estimator
            .estimate_prices(&sell_tokens, self.denominator_token)
            .await
            .into_iter()
            .zip(sell_tokens)
            .filter_map(|(result, token)| {
                if let Ok(price) = result {
                    Some((token, price))
                } else {
                    None
                }
            })
            .collect();

        orders
            .into_iter()
            .filter(|order| {
                if let Liquidity::Limit(order) = order {
                    prices
                        .get(&order.sell_token)
                        .map(|price| {
                            price * order.sell_amount.to_big_rational()
                                > self.min_value.to_big_rational()
                        })
                        .unwrap_or(false)
                } else {
                    true
                }
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl Solver for SellVolumeFilteringSolver {
    async fn solve(&self, orders: Vec<Liquidity>, gas_price: f64) -> Result<Vec<Settlement>> {
        let original_length = orders.len();
        let filtered_liquidity = self.filter_liquidity(orders).await;
        tracing::info!(
            "Filtered {} orders because on insufficient volume",
            original_length - filtered_liquidity.len()
        );
        self.inner.solve(filtered_liquidity, gas_price).await
    }

    fn account(&self) -> &Account {
        self.inner.account()
    }

    fn name(&self) -> &'static str {
        self.inner.name()
    }
}

#[cfg(test)]
mod tests {
    use num::BigRational;
    use shared::price_estimate::mocks::{FailingPriceEstimator, FakePriceEstimator};

    use crate::liquidity::LimitOrder;

    use super::*;

    /// Dummy solver returning no settlements
    pub struct NoopSolver();
    #[async_trait::async_trait]
    impl Solver for NoopSolver {
        async fn solve(&self, _: Vec<Liquidity>, _: f64) -> Result<Vec<Settlement>> {
            Ok(Vec::new())
        }

        fn account(&self) -> &Account {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn test_filtering_solver_removes_limit_orders_with_too_little_volume() {
        let sell_token = H160::from_low_u64_be(1);
        let liquidity = vec![
            // Only filter limit orders
            Liquidity::ConstantProduct(Default::default()),
            // Orders with high enough amount
            Liquidity::Limit(LimitOrder {
                sell_amount: 100_000.into(),
                sell_token,
                ..Default::default()
            }),
            Liquidity::Limit(LimitOrder {
                sell_amount: 500_000.into(),
                sell_token,
                ..Default::default()
            }),
            // Order with small amount
            Liquidity::Limit(LimitOrder {
                sell_amount: 100.into(),
                sell_token,
                ..Default::default()
            }),
        ];

        let price_estimator = Arc::new(FakePriceEstimator(BigRational::from_integer(42.into())));
        let solver = SellVolumeFilteringSolver {
            inner: Box::new(NoopSolver()),
            price_estimator,
            denominator_token: H160::zero(),
            min_value: 400_000.into(),
        };
        assert_eq!(solver.filter_liquidity(liquidity).await.len(), 3);
    }

    #[tokio::test]
    async fn test_filtering_solver_removes_orders_without_price_estimate() {
        let sell_token = H160::from_low_u64_be(1);
        let liquidity = vec![Liquidity::Limit(LimitOrder {
            sell_amount: 100_000.into(),
            sell_token,
            ..Default::default()
        })];

        let price_estimator = Arc::new(FailingPriceEstimator());
        let solver = SellVolumeFilteringSolver {
            inner: Box::new(NoopSolver()),
            price_estimator,
            denominator_token: H160::zero(),
            min_value: 0.into(),
        };
        assert_eq!(solver.filter_liquidity(liquidity).await.len(), 0);
    }
}
