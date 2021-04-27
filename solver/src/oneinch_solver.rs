//! Module containing implementation of the 1Inch solver.
//!
//! This simple solver will simply use the 1Inch API to get a quote for a
//! single GPv2 order and produce a settlement directly against 1Inch.

pub mod api;

use self::api::{OneInchClient, Slippage, Swap, SwapQuery};
use crate::{
    encoding::EncodedInteraction,
    interactions::Erc20ApproveInteraction,
    liquidity::{slippage::MAX_SLIPPAGE_BPS, LimitOrder, Liquidity},
    settlement::{Interaction, Settlement},
    solver::Solver,
};
use anyhow::Result;
use contracts::{GPv2Settlement, ERC20};
use ethcontract::U256;
use maplit::hashmap;
use model::order::OrderKind;
use shared::Web3;
use std::fmt::{self, Display, Formatter};

/// A GPv2 solver that matches GP **sell** orders to direct 1Inch swaps.
#[derive(Debug)]
pub struct OneInchSolver {
    web3: Web3,
    settlement_contract: GPv2Settlement,
    client: OneInchClient,
}

impl OneInchSolver {
    /// Creates a new 1Inch solver instance for specified settlement contract
    /// instance.
    pub fn new(web3: Web3, settlement_contract: GPv2Settlement) -> Self {
        Self {
            web3,
            settlement_contract,
            client: Default::default(),
        }
    }

    /// Settles a single sell order against a 1Inch swap.
    async fn settle_order(&self, order: &LimitOrder) -> Result<Option<Settlement>> {
        debug_assert_eq!(
            order.kind,
            OrderKind::Sell,
            "only sell orders should be passed to settle_order"
        );

        let spender = self.client.get_spender().await?;
        let sell_token = ERC20::at(&self.web3, order.sell_token);
        let existing_allowance = sell_token
            .allowance(self.settlement_contract.address(), spender.address)
            .call()
            .await?;

        let swap = match self
            .client
            .get_swap(SwapQuery {
                from_token_address: order.sell_token,
                to_token_address: order.buy_token,
                amount: order.sell_amount,
                from_address: self.settlement_contract.address(),
                slippage: Slippage::basis_points(MAX_SLIPPAGE_BPS).unwrap(),
                // Disable balance/allowance checks, as the settlement contract
                // does not hold balances to traded tokens.
                disable_estimate: Some(true),
            })
            .await
        {
            Ok(swap) => swap,
            Err(err) => {
                // It could be that 1Inch can't find match an order and would
                // return an error for whatever reason. In that case, we want to
                // continue trying to solve for other orders.
                tracing::warn!("1Inch API error quoting swap: {}", err);
                return Ok(None);
            }
        };

        let mut settlement = Settlement::new(hashmap! {
            order.sell_token => swap.to_token_amount,
            order.buy_token => swap.from_token_amount,
        });

        settlement.with_liquidity(order, order.sell_amount)?;

        if existing_allowance < order.sell_amount {
            settlement
                .encoder
                .append_to_execution_plan(Erc20ApproveInteraction {
                    token: sell_token,
                    owner: self.settlement_contract.address(),
                    spender: spender.address,
                    amount: U256::MAX,
                });
        }
        settlement.encoder.append_to_execution_plan(swap);

        Ok(None)
    }
}

impl Interaction for Swap {
    fn encode(&self) -> Vec<EncodedInteraction> {
        vec![(self.tx.to, self.tx.value, self.tx.data.clone())]
    }
}

#[async_trait::async_trait]
impl Solver for OneInchSolver {
    async fn solve(
        &self,
        liquidity: Vec<Liquidity>,
        _gas_price: f64,
    ) -> Result<Option<Settlement>> {
        let sell_orders = liquidity
            .into_iter()
            .filter_map(|liquidity| match liquidity {
                Liquidity::Limit(order) if order.kind == OrderKind::Sell => Some(order),
                _ => None,
            });

        for order in sell_orders {
            let settlement = self.settle_order(&order).await?;
            if settlement.is_some() {
                return Ok(settlement);
            }
        }

        Ok(None)
    }
}

impl Display for OneInchSolver {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "OneInchSolver")
    }
}
