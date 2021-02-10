mod model;

use self::model::*;
use crate::{liquidity::Liquidity, settlement::Settlement, solver::Solver};
use ::model::order::OrderKind;
use anyhow::{ensure, Context, Result};
use primitive_types::H160;
use reqwest::{Client, Url};
use std::collections::{HashMap, HashSet};

/// The configuration passed as url parameters to the solver.
#[derive(Debug, Default)]
pub struct SolverConfig {
    max_nr_exec_orders: u32,
    time_limit: u32,
    // TODO: add more parameters that we want to set
}

impl SolverConfig {
    fn add_to_query(&self, url: &mut Url) {
        url.query_pairs_mut()
            .append_pair(
                "max_nr_exec_orders",
                self.max_nr_exec_orders.to_string().as_str(),
            )
            .append_pair("time_limit", self.time_limit.to_string().as_str());
    }
}

pub struct HttpSolver {
    base: Url,
    client: Client,
    api_key: Option<String>,
    config: SolverConfig,
}

impl HttpSolver {
    pub fn new(base: Url, api_key: Option<String>, config: SolverConfig) -> Self {
        // Unwrap because we cannot handle client creation failing.
        let client = Client::builder().build().unwrap();
        Self {
            base,
            client,
            api_key,
            config,
        }
    }

    // Solver api requires specifying token as strings. We use the address as a string for now.
    // Later we could use a more meaningful name like the token symbol but we have to ensure
    // uniqueness.
    fn token_to_string(&self, token: &H160) -> String {
        // Token names must start with a letter.
        format!("t{:x}", token)
    }

    fn tokens(&self, orders: &[Liquidity]) -> HashMap<String, TokenInfoModel> {
        orders
            .iter()
            .flat_map(|liquidity| match liquidity {
                Liquidity::Limit(order) => {
                    std::iter::once(order.sell_token).chain(std::iter::once(order.buy_token))
                }
                Liquidity::Amm(amm) => {
                    std::iter::once(amm.tokens.get().0).chain(std::iter::once(amm.tokens.get().1))
                }
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|token| {
                // TODO: gather real decimals and store them in a cache
                let token_model = TokenInfoModel { decimals: 18 };
                (self.token_to_string(&token), token_model)
            })
            .collect()
    }

    fn orders(&self, orders: &[Liquidity]) -> HashMap<String, OrderModel> {
        orders
            .iter()
            .filter_map(|liquidity| match liquidity {
                Liquidity::Limit(order) => Some(order),
                Liquidity::Amm(_) => None,
            })
            .enumerate()
            .map(|(index, order)| {
                let order = OrderModel {
                    sell_token: self.token_to_string(&order.sell_token),
                    buy_token: self.token_to_string(&order.buy_token),
                    sell_amount: order.sell_amount,
                    buy_amount: order.buy_amount,
                    allow_partial_fill: order.partially_fillable,
                    is_sell_order: matches!(order.kind, OrderKind::Sell),
                };
                (index.to_string(), order)
            })
            .collect()
    }

    async fn uniswaps(&self, orders: &[Liquidity]) -> Result<HashMap<String, UniswapModel>> {
        // TODO: use a cache
        Ok(orders
            .iter()
            .filter_map(|liquidity| match liquidity {
                Liquidity::Limit(_) => None,
                Liquidity::Amm(amm) => Some(amm),
            })
            .enumerate()
            .map(|(index, amm)| {
                let uniswap = UniswapModel {
                    token1: self.token_to_string(&amm.tokens.get().0),
                    token2: self.token_to_string(&amm.tokens.get().1),
                    balance1: amm.reserves.0,
                    balance2: amm.reserves.1,
                    // TODO use AMM fee
                    fee: 0.003,
                    mandatory: false,
                };
                (index.to_string(), uniswap)
            })
            .collect())
    }

    async fn create_body(&self, orders: &[Liquidity]) -> Result<BatchAuctionModel> {
        Ok(BatchAuctionModel {
            tokens: self.tokens(orders),
            orders: self.orders(orders),
            uniswaps: self.uniswaps(orders).await?,
            ref_token: self.token_to_string(&H160::zero()),
            default_fee: 0.0,
        })
    }

    async fn send(&self, model: &BatchAuctionModel) -> Result<SettledBatchAuctionModel> {
        let mut url = self.base.clone();
        url.set_path("/solve");
        self.config.add_to_query(&mut url);
        let query = url.query().map(ToString::to_string).unwrap_or_default();
        let mut request = self.client.post(url);
        if let Some(api_key) = &self.api_key {
            request = request.header("X-API-KEY", api_key);
        }
        let body = serde_json::to_string(&model).context("failed to encode body")?;
        tracing::debug!("request query {} body {}", query, body);
        let request = request.body(body);

        let response = request.send().await.context("failed to send request")?;
        let status = response.status();
        let body_bytes = response
            .bytes()
            .await
            .context("failed to get response body")?;
        let body_str =
            std::str::from_utf8(body_bytes.as_ref()).context("failed to decode response body")?;
        tracing::debug!("response body {}", body_str);
        ensure!(
            status.is_success(),
            "solver response is not success: status: {}",
            status
        );
        serde_json::from_str(body_str).context("failed to decode response json")
    }
}

#[async_trait::async_trait]
impl Solver for HttpSolver {
    async fn solve(&self, orders: Vec<Liquidity>) -> Result<Option<Settlement>> {
        let body = self.create_body(orders.as_slice()).await?;
        self.send(&body).await?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use crate::liquidity::{LimitOrder, MockLimitOrderSettlementHandling};
    use std::sync::Arc;

    use super::*;

    // cargo test real_solver -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn real_solver() {
        tracing_subscriber::fmt::fmt()
            .with_env_filter("debug")
            .init();
        let solver = HttpSolver::new(
            "http://localhost:8000".parse().unwrap(),
            None,
            SolverConfig {
                max_nr_exec_orders: 100,
                time_limit: 100,
            },
        );
        let orders = vec![Liquidity::Limit(LimitOrder {
            buy_token: H160::zero(),
            sell_token: H160::from_low_u64_be(1),
            buy_amount: 1.into(),
            sell_amount: 1.into(),
            kind: OrderKind::Sell,
            partially_fillable: false,
            settlement_handling: Arc::new(MockLimitOrderSettlementHandling::new()),
        })];
        solver.solve(orders).await.unwrap();
    }
}
