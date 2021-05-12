//! Module containing implementation of the 1Inch solver.
//!
//! This simple solver will simply use the 1Inch API to get a quote for a
//! single GPv2 order and produce a settlement directly against 1Inch.

pub mod api;

use self::api::{OneInchClient, Slippage, Swap, SwapQuery};
use crate::{
    encoding::EncodedInteraction,
    interactions::Erc20ApproveInteraction,
    intermediate_settlement::{
        ExecutedLiquidityOrder, IntermediateSettlement, SettlementFinalizing,
    },
    liquidity::{slippage::MAX_SLIPPAGE_BPS, LimitOrder, Liquidity},
    settlement::{Interaction, Settlement},
    solver::Solver,
};
use anyhow::{ensure, Result};
use contracts::{GPv2Settlement, ERC20};
use ethcontract::{dyns::DynWeb3, H160, U256};
use futures::future;
use maplit::hashmap;
use model::order::OrderKind;
use rand::seq::SliceRandom as _;
use std::fmt::{self, Display, Formatter};

/// A GPv2 solver that matches GP **sell** orders to direct 1Inch swaps.
#[derive(Debug)]
pub struct OneInchSolver {
    settlement_contract: GPv2Settlement,
    client: OneInchClient,
}

/// Chain ID for Mainnet.
const MAINNET_CHAIN_ID: u64 = 1;

impl OneInchSolver {
    /// Creates a new 1Inch solver instance for specified settlement contract
    /// instance.
    pub fn new(settlement_contract: GPv2Settlement, chain_id: u64) -> Result<Self> {
        ensure!(
            chain_id == MAINNET_CHAIN_ID,
            "1Inch solver only supported on Mainnet",
        );

        Ok(Self {
            settlement_contract,
            client: Default::default(),
        })
    }

    async fn get_swap(&self, sell_token: H160, buy_token: H160, sell_amount: U256) -> Result<Swap> {
        self.client
            .get_swap(SwapQuery {
                from_token_address: sell_token,
                to_token_address: buy_token,
                amount: sell_amount,
                from_address: self.settlement_contract.address(),
                slippage: Slippage::basis_points(MAX_SLIPPAGE_BPS).unwrap(),
                // Disable balance/allowance checks, as the settlement contract
                // does not hold balances to traded tokens.
                disable_estimate: Some(true),
            })
            .await
    }

    /// Settles a single sell order against a 1Inch swap.
    async fn settle_order(&self, order: LimitOrder) -> Result<IntermediateSettlement> {
        debug_assert_eq!(
            order.kind,
            OrderKind::Sell,
            "only sell orders should be passed to settle_order"
        );

        let swap = self
            .get_swap(order.sell_token, order.buy_token, order.sell_amount)
            .await?;

        ensure!(
            swap.to_token_amount >= order.buy_amount,
            "order limit price not respected",
        );

        let prices = hashmap! {
            order.sell_token => swap.to_token_amount,
            order.buy_token => swap.from_token_amount,
        };
        let mut intermediate_settlement = IntermediateSettlement::new(prices);

        intermediate_settlement.add_fully_executed_limit_order(order.clone());

        intermediate_settlement.add_executed_liquidity_order(
            (order.sell_token, swap.from_token_amount),
            (order.buy_token, swap.to_token_amount),
            Box::new(swap),
        );

        Ok(intermediate_settlement)
    }

    fn web3(&self) -> DynWeb3 {
        self.settlement_contract.raw_instance().web3()
    }
}

struct OneInchSettlementFinalizer<'a> {
    solver: &'a OneInchSolver,
}

#[async_trait::async_trait(?Send)]
impl SettlementFinalizing for OneInchSettlementFinalizer<'_> {
    async fn finalize_intermediate_liquidity_order(
        &self,
        settlement: &mut Settlement,
        liquidity_order: &ExecutedLiquidityOrder,
    ) -> Result<()> {
        let spender = self.solver.client.get_spender().await?;

        let sell_token = liquidity_order.input.0;
        let buy_token = liquidity_order.output.0;
        let sell_token_contract = ERC20::at(&self.solver.web3(), sell_token);
        let existing_allowance = sell_token_contract
            .allowance(self.solver.settlement_contract.address(), spender.address)
            .call()
            .await?;

        let sell_amount = liquidity_order.input.1;

        let mut swap = liquidity_order
            .order
            .downcast_ref::<Swap>()
            .unwrap()
            .clone();

        if swap.from_token_amount != sell_amount {
            // Must replay swap since sell amount changed
            swap = self
                .solver
                .get_swap(sell_token, buy_token, sell_amount)
                .await?;
        }

        let buy_amount = liquidity_order.output.1;
        ensure!(
            swap.to_token_amount >= buy_amount,
            "can't replay 1inch swap with same limit price",
        );

        if existing_allowance < sell_amount {
            settlement
                .encoder
                .append_to_execution_plan(Erc20ApproveInteraction {
                    token: sell_token_contract,
                    spender: spender.address,
                    amount: U256::MAX,
                });
        }
        settlement.encoder.append_to_execution_plan(swap);

        Ok(())
    }
}

impl Interaction for Swap {
    fn encode(&self) -> Vec<EncodedInteraction> {
        vec![(self.tx.to, self.tx.value, self.tx.data.clone())]
    }
}

/// Maximum number of sell orders to consider for settlements.
///
/// This is mostly out of concern to avoid rate limiting and because 1Inch
/// requests take a non-trivial amount of time.
const MAX_SETTLEMENTS: usize = 5;

#[async_trait::async_trait]
impl Solver for OneInchSolver {
    async fn solve(
        &self,
        liquidity: Vec<Liquidity>,
        _gas_price: f64,
    ) -> Result<Vec<IntermediateSettlement>> {
        let mut sell_orders = liquidity
            .into_iter()
            .filter_map(|liquidity| match liquidity {
                Liquidity::Limit(order) if order.kind == OrderKind::Sell => Some(order),
                _ => None,
            })
            .collect::<Vec<_>>();

        // Randomize which orders we take, this prevents this solver "getting
        // stuck" on bad orders.
        if sell_orders.len() > MAX_SETTLEMENTS {
            sell_orders.shuffle(&mut rand::thread_rng());
        }

        let settlements = future::join_all(
            sell_orders
                .into_iter()
                .take(MAX_SETTLEMENTS)
                .map(|sell_order| self.settle_order(sell_order)),
        )
        .await;

        Ok(settlements
            .into_iter()
            .filter_map(|settlement| match settlement {
                Ok(settlement) => Some(settlement),
                Err(err) => {
                    // It could be that 1Inch can't match an order and would
                    // return an error for whatever reason. In that case, we want
                    // to continue trying to solve for other orders.
                    tracing::warn!("1Inch API error quoting swap: {}", err);
                    None
                }
            })
            .collect())
    }
}

impl Display for OneInchSolver {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "OneInchSolver")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        liquidity::{AmmOrder, LimitOrder},
        testutil,
    };
    use contracts::{GPv2Settlement, WETH9};
    use ethcontract::H160;
    use model::order::{Order, OrderCreation, OrderKind};

    fn dummy_solver() -> OneInchSolver {
        let web3 = testutil::dummy_web3();
        let settlement = GPv2Settlement::at(&web3, H160::zero());
        OneInchSolver::new(settlement, MAINNET_CHAIN_ID).unwrap()
    }

    #[tokio::test]
    #[cfg(debug_assertions)]
    #[should_panic]
    async fn panics_when_settling_buy_orders() {
        let _ = dummy_solver()
            .settle_order(LimitOrder {
                kind: OrderKind::Buy,
                ..Default::default()
            })
            .await;
    }

    #[tokio::test]
    async fn ignores_all_liquidity_other_than_sell_orders() {
        let settlements = dummy_solver()
            .solve(
                vec![
                    Liquidity::Limit(LimitOrder {
                        kind: OrderKind::Buy,
                        ..Default::default()
                    }),
                    Liquidity::Amm(AmmOrder::default()),
                ],
                0.0,
            )
            .await
            .unwrap();

        assert_eq!(settlements.len(), 0);
    }

    #[tokio::test]
    #[ignore]
    async fn solve_order_on_oneinch() {
        let web3 = testutil::infura("mainnet");
        let chain_id = web3.eth().chain_id().await.unwrap().as_u64();
        let settlement = GPv2Settlement::deployed(&web3).await.unwrap();

        let weth = WETH9::deployed(&web3).await.unwrap();
        let gno = addr!("6810e776880c02933d47db1b9fc05908e5386b96");

        let solver = OneInchSolver::new(settlement, chain_id).unwrap();
        let settlement = solver
            .settle_order(
                Order {
                    order_creation: OrderCreation {
                        sell_token: weth.address(),
                        buy_token: gno,
                        sell_amount: 1_000_000_000_000_000_000u128.into(),
                        buy_amount: 1u128.into(),
                        kind: OrderKind::Sell,
                        ..Default::default()
                    },
                    ..Default::default()
                }
                .into(),
            )
            .await
            .unwrap();

        println!("{:#?}", settlement);
    }

    #[tokio::test]
    #[ignore]
    async fn returns_error_on_non_mainnet() {
        let web3 = testutil::infura("rinkeby");
        let chain_id = web3.eth().chain_id().await.unwrap().as_u64();
        let settlement = GPv2Settlement::deployed(&web3).await.unwrap();

        assert!(OneInchSolver::new(settlement, chain_id).is_err())
    }
}
