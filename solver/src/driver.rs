use crate::{
    liquidity::{uniswap::UniswapLiquidity, Liquidity},
    orderbook::OrderBookApi,
    settlement::Settlement,
    settlement_submission,
    solver::Solver,
};
use anyhow::{Context, Result};
use contracts::GPv2Settlement;
use futures::future::join_all;
use gas_estimation::GasPriceEstimating;
use orderbook::price_estimate::UniswapPriceEstimator;
use std::{cmp::Reverse, time::Duration};
use tracing::info;

// There is no economic viability calculation yet so we're using an arbitrary very high cap to
// protect against a gas estimator giving bogus results that would drain all our funds.
const GAS_PRICE_CAP: f64 = 500e9;

pub struct Driver {
    settlement_contract: GPv2Settlement,
    orderbook: OrderBookApi,
    uniswap_liquidity: UniswapLiquidity,
    price_estimator: UniswapPriceEstimator,
    solver: Vec<Box<dyn Solver>>,
    gas_price_estimator: Box<dyn GasPriceEstimating>,
    target_confirm_time: Duration,
    settle_interval: Duration,
}

impl Driver {
    pub fn new(
        settlement_contract: GPv2Settlement,
        uniswap_liquidity: UniswapLiquidity,
        orderbook: OrderBookApi,
        price_estimator: UniswapPriceEstimator,
        solver: Vec<Box<dyn Solver>>,
        gas_price_estimator: Box<dyn GasPriceEstimating>,
        target_confirm_time: Duration,
        settle_interval: Duration,
    ) -> Self {
        Self {
            settlement_contract,
            orderbook,
            uniswap_liquidity,
            price_estimator,
            solver,
            gas_price_estimator,
            target_confirm_time,
            settle_interval,
        }
    }

    pub async fn run_forever(&mut self) -> ! {
        loop {
            match self.single_run().await {
                Ok(()) => tracing::debug!("single run finished ok"),
                Err(err) => tracing::error!("single run errored: {:?}", err),
            }
            tokio::time::delay_for(self.settle_interval).await;
        }
    }

    pub async fn single_run(&mut self) -> Result<()> {
        tracing::debug!("starting single run");
        let limit_orders = self
            .orderbook
            .get_liquidity()
            .await
            .context("failed to get orderbook")?;
        tracing::debug!("got {} orders", limit_orders.len());

        let amms = self
            .uniswap_liquidity
            .get_liquidity(limit_orders.iter())
            .await
            .context("failed to get uniswap pools")?;
        tracing::debug!("got {} AMMs", amms.len());

        let liquidity: Vec<Liquidity> = limit_orders
            .into_iter()
            .map(Liquidity::Limit)
            .chain(amms.into_iter().map(Liquidity::Amm))
            .collect();

        /*
        // Computes set of traded tokens (limit orders only).
        let tokens = limit_orders
            .into_iter()
            .flat_map(|lo| vec![lo.sell_token, lo.buy_token].iter())
            .sorted()
            .dedup();

        web3 = ...

        let native_token = WETH9::deployed(&web3)
            .await
            .expect("couldn't load deployed native token");
        let estimated_prices = self.price_estimator.best_execution_spot_prices(
            tokens, native_token)
        todo: turn estimated_prices into a map token->price
        */

        let mut settlements: Vec<(&Box<dyn Solver>, Settlement)> =
            join_all(self.solver.iter().map(|solver| {
                let liquidity = liquidity.clone();
                async move { (solver, solver.solve(liquidity).await) }
            }))
            .await
            .into_iter()
            .filter_map(|(solver, settlement)| {
                let settlement = settlement.ok()??;
                info!(
                    "{} found solution with objective value: {}",
                    solver,
                    settlement.objective_value(/* estimated_prices*/)
                );
                Some((solver, settlement))
            })
            .collect();

        // Sort by key in descending order
        settlements.sort_by_key(|(_, settlement)| Reverse(settlement.objective_value(/* estimated_prices*/)));
        for (solver, settlement) in settlements {
            info!("{} computed {:?}", solver, settlement);
            if settlement.trades.is_empty() {
                info!("Skipping empty settlement");
                continue;
            }
            match settlement_submission::submit(
                &self.settlement_contract,
                self.gas_price_estimator.as_ref(),
                self.target_confirm_time,
                GAS_PRICE_CAP,
                settlement,
            )
            .await
            {
                Ok(_) => {
                    // TODO: order validity checks
                    // Decide what is handled by orderbook service and what by us.
                    // We likely want to at least mark orders we know we have settled so that we don't
                    // attempt to settle them again when they are still in the orderbook.
                    break;
                }
                Err(err) => tracing::error!("{} Failed to submit settlement: {:?}", solver, err),
            }
        }
        Ok(())
    }
}
