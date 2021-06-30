//! Module containing implementation of the Matcha solver.
//!
//! This solver will simply use the Matcha API to get a quote for a
//! single GPv2 order and produce a settlement directly against Matcha.

pub mod api;

use super::solver_utils::Slippage;
use crate::interactions::allowances::{AllowanceManager, AllowanceManaging};
use crate::solver::matcha_solver::api::MatchaApi;
use anyhow::{ensure, Result};
use contracts::GPv2Settlement;
use ethcontract::Bytes;
use maplit::hashmap;

use super::single_order_solver::SingleOrderSolving;

use self::api::{DefaultMatchaApi, SwapQuery, SwapResponse};
use crate::{
    encoding::EncodedInteraction,
    liquidity::LimitOrder,
    settlement::{Interaction, Settlement},
};
use model::order::OrderKind;
use shared::Web3;
use std::fmt::{self, Display, Formatter};

/// Constant maximum slippage of 5 BPS (0.05%) to use for on-chain liquidity.
/// This is half the 1inch slippage
pub const STANDARD_MATCHA_SLIPPAGE_BPS: u16 = 5;

/// A GPv2 solver that matches GP orders to direct Matcha swaps.
pub struct MatchaSolver {
    client: Box<dyn MatchaApi + Send + Sync>,
    allowance_fetcher: Box<dyn AllowanceManaging>,
}

/// Chain ID for Mainnet.
const MAINNET_CHAIN_ID: u64 = 1;

impl MatchaSolver {
    pub fn new(web3: Web3, settlement_contract: GPv2Settlement, chain_id: u64) -> Result<Self> {
        ensure!(
            chain_id == MAINNET_CHAIN_ID,
            "Matcha solver only supported on Mainnet",
        );
        let allowance_fetcher = AllowanceManager::new(web3, settlement_contract.address());
        Ok(Self {
            allowance_fetcher: Box::new(allowance_fetcher),
            client: Box::new(DefaultMatchaApi::default()),
        })
    }
}

#[async_trait::async_trait]
impl SingleOrderSolving for MatchaSolver {
    async fn settle_order(&self, order: LimitOrder) -> Result<Option<Settlement>> {
        let swap = match order.kind {
            OrderKind::Sell => {
                let query = SwapQuery {
                    sell_token: order.sell_token,
                    buy_token: order.buy_token,
                    sell_amount: Some(order.sell_amount),
                    buy_amount: None,
                    slippage_percentage: Slippage::number_from_basis_points(
                        STANDARD_MATCHA_SLIPPAGE_BPS,
                    )
                    .unwrap(),
                    skip_validation: Some(true),
                };

                tracing::debug!("querying Matcha swap api with {:?}", query);
                let swap = self.client.get_swap(query).await?;
                tracing::debug!("proposed Matcha swap is {:?}", swap);

                if swap.buy_amount < order.buy_amount {
                    tracing::debug!("Order limit price not respected");
                    return Ok(None);
                }
                swap
            }
            OrderKind::Buy => {
                let query = SwapQuery {
                    sell_token: order.sell_token,
                    buy_token: order.buy_token,
                    sell_amount: None,
                    buy_amount: Some(order.buy_amount),
                    slippage_percentage: Slippage::number_from_basis_points(
                        STANDARD_MATCHA_SLIPPAGE_BPS,
                    )
                    .unwrap(),
                    // From the api documentation:
                    // SlippagePercentage(Optional): The maximum acceptable slippage in % of the buyToken amount if sellAmount is provided, the maximum acceptable slippage in % of the sellAmount amount if buyAmount is provided. This parameter will change over time with market conditions.
                    // => Hence, allegedly, we don't need to build in buffers ourselves. Though the sell amount is not adjusted, if slippage is changed for buy order requests.
                    // Todo: Continue discussion with the 0x-team
                    skip_validation: Some(true),
                };

                tracing::debug!("querying Matcha swap api with {:?}", query);
                let swap = self.client.get_swap(query).await?;
                tracing::debug!("proposed Matcha swap is {:?}", swap);

                if swap.sell_amount > order.sell_amount {
                    tracing::debug!("Order limit price not respected");
                    return Ok(None);
                }
                swap
            }
        };
        let mut settlement = Settlement::new(hashmap! {
            order.sell_token => swap.buy_amount,
            order.buy_token => swap.sell_amount,
        });
        let spender = swap.allowance_target;

        settlement.with_liquidity(&order, swap.sell_amount)?;

        settlement.encoder.append_to_execution_plan(
            self.allowance_fetcher
                .get_approval(order.sell_token, spender, swap.sell_amount)
                .await?,
        );
        settlement.encoder.append_to_execution_plan(swap);
        Ok(Some(settlement))
    }

    fn name(&self) -> &'static str {
        "Matcha"
    }
}

impl Interaction for SwapResponse {
    fn encode(&self) -> Vec<EncodedInteraction> {
        vec![(self.to, self.value, Bytes(self.data.0.clone()))]
    }
}

impl Display for MatchaSolver {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "MatchaSolver")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interactions::allowances::{Approval, MockAllowanceManaging};
    use crate::liquidity::LimitOrder;
    use crate::solver::matcha_solver::api::MockMatchaApi;
    use contracts::{GPv2Settlement, WETH9};
    use ethcontract::{Web3, H160, U256};
    use mockall::predicate::*;
    use mockall::Sequence;
    use model::order::{Order, OrderCreation, OrderKind};
    use shared::transport::{create_env_test_transport, create_test_transport};

    #[tokio::test]
    #[ignore]
    async fn solve_sell_order_on_matcha() {
        let web3 = Web3::new(create_env_test_transport());
        let chain_id = web3.eth().chain_id().await.unwrap().as_u64();
        let settlement = GPv2Settlement::deployed(&web3).await.unwrap();

        let weth = WETH9::deployed(&web3).await.unwrap();
        let gno = shared::addr!("6810e776880c02933d47db1b9fc05908e5386b96");

        let solver = MatchaSolver::new(web3, settlement, chain_id).unwrap();
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
    async fn solve_buy_order_on_matcha() {
        let web3 = Web3::new(create_env_test_transport());
        let chain_id = web3.eth().chain_id().await.unwrap().as_u64();
        let settlement = GPv2Settlement::deployed(&web3).await.unwrap();

        let weth = WETH9::deployed(&web3).await.unwrap();
        let gno = shared::addr!("6810e776880c02933d47db1b9fc05908e5386b96");

        let solver = MatchaSolver::new(web3, settlement, chain_id).unwrap();
        let settlement = solver
            .settle_order(
                Order {
                    order_creation: OrderCreation {
                        sell_token: weth.address(),
                        buy_token: gno,
                        sell_amount: 1_000_000_000_000_000_000u128.into(),
                        buy_amount: 1_000_000_000_000_000_000u128.into(),
                        kind: OrderKind::Buy,
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
    async fn test_satisfies_limit_price_for_sell_order() {
        let mut client = Box::new(MockMatchaApi::new());
        let mut allowance_fetcher = Box::new(MockAllowanceManaging::new());

        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(1);

        let allowance_target = shared::addr!("def1c0ded9bec7f1a1670819833240f027b25eff");
        client.expect_get_swap().returning(move|_| {
            Ok(SwapResponse {
            sell_amount: U256::from_dec_str("100").unwrap(),
             buy_amount: U256::from_dec_str("91").unwrap(),
             allowance_target,
            price: 0.91_f64,
            to: shared::addr!("def1c0ded9bec7f1a1670819833240f027b25eff"),
            data: web3::types::Bytes(hex::decode(
                "d9627aa40000000000000000000000000000000000000000000000000000000000000080000000000000000000000000000000000000000000000000016345785d8a00000000000000000000000000000000000000000000000000001206e6c0056936e100000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20000000000000000000000006810e776880c02933d47db1b9fc05908e5386b96869584cd0000000000000000000000001000000000000000000000000000000000000011000000000000000000000000000000000000000000000092415e982f60d431ba"
            ).unwrap()),
            value: U256::from_dec_str("0").unwrap(),
        })});

        allowance_fetcher
            .expect_get_approval()
            .times(1)
            .with(eq(sell_token), eq(allowance_target), eq(U256::from(100)))
            .returning(move |_, _, _| {
                Ok(Approval::Approve {
                    token: sell_token,
                    spender: allowance_target,
                })
            });

        let solver = MatchaSolver {
            client,
            allowance_fetcher,
        };

        let order_passing_limit = LimitOrder {
            sell_token,
            buy_token,
            sell_amount: 100.into(),
            buy_amount: 90.into(),
            kind: model::order::OrderKind::Sell,
            ..Default::default()
        };
        let order_violating_limit = LimitOrder {
            sell_token,
            buy_token,
            sell_amount: 100.into(),
            buy_amount: 110.into(),
            kind: model::order::OrderKind::Sell,
            ..Default::default()
        };

        let result = solver
            .settle_order(order_passing_limit)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.clearing_prices(),
            &hashmap! {
                sell_token => 91.into(),
                buy_token => 100.into(),
            }
        );

        let result = solver.settle_order(order_violating_limit).await.unwrap();
        assert!(result.is_none());
    }
    #[tokio::test]
    async fn test_satisfies_limit_price_for_buy_order() {
        let mut client = Box::new(MockMatchaApi::new());
        let mut allowance_fetcher = Box::new(MockAllowanceManaging::new());

        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(1);

        let allowance_target = shared::addr!("def1c0ded9bec7f1a1670819833240f027b25eff");
        client.expect_get_swap().returning(move|_| {
            Ok(SwapResponse {
            sell_amount: U256::from_dec_str("100").unwrap(),
             buy_amount: U256::from_dec_str("91").unwrap(),
             allowance_target,
            price: 0.91_f64,
            to: shared::addr!("def1c0ded9bec7f1a1670819833240f027b25eff"),
            data: web3::types::Bytes(hex::decode(
                "d9627aa40000000000000000000000000000000000000000000000000000000000000080000000000000000000000000000000000000000000000000016345785d8a00000000000000000000000000000000000000000000000000001206e6c0056936e100000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20000000000000000000000006810e776880c02933d47db1b9fc05908e5386b96869584cd0000000000000000000000001000000000000000000000000000000000000011000000000000000000000000000000000000000000000092415e982f60d431ba"
            ).unwrap()),
            value: U256::from_dec_str("0").unwrap(),
        })});

        allowance_fetcher
            .expect_get_approval()
            .times(1)
            .with(eq(sell_token), eq(allowance_target), eq(U256::from(100)))
            .returning(move |_, _, _| {
                Ok(Approval::Approve {
                    token: sell_token,
                    spender: allowance_target,
                })
            });

        let solver = MatchaSolver {
            client,
            allowance_fetcher,
        };

        let order_passing_limit = LimitOrder {
            sell_token,
            buy_token,
            sell_amount: 101.into(),
            buy_amount: 91.into(),
            kind: model::order::OrderKind::Buy,
            ..Default::default()
        };
        let order_violating_limit = LimitOrder {
            sell_token,
            buy_token,
            sell_amount: 99.into(),
            buy_amount: 91.into(),
            kind: model::order::OrderKind::Buy,
            ..Default::default()
        };

        let result = solver
            .settle_order(order_passing_limit)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.clearing_prices(),
            &hashmap! {
                sell_token => 91.into(),
                buy_token => 100.into(),
            }
        );

        let result = solver.settle_order(order_violating_limit).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    #[ignore]
    async fn returns_error_on_non_mainnet() {
        let web3 = Web3::new(create_test_transport(
            &std::env::var("NODE_URL_RINKEBY").unwrap(),
        ));
        let chain_id = web3.eth().chain_id().await.unwrap().as_u64();
        let settlement = GPv2Settlement::deployed(&web3).await.unwrap();

        assert!(MatchaSolver::new(web3, settlement, chain_id).is_err())
    }

    #[tokio::test]
    async fn test_sets_allowance_if_necessary() {
        let mut client = Box::new(MockMatchaApi::new());
        let mut allowance_fetcher = Box::new(MockAllowanceManaging::new());

        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(1);

        let allowance_target = shared::addr!("def1c0ded9bec7f1a1670819833240f027b25eff");
        client.expect_get_swap().returning(move |_| {
            Ok(SwapResponse {
                sell_amount: U256::from_dec_str("100").unwrap(),
                 buy_amount: U256::from_dec_str("91").unwrap(),
                 allowance_target ,
                price: 13.121_002_575_170_278_f64,
                to: shared::addr!("def1c0ded9bec7f1a1670819833240f027b25eff"),
                data: web3::types::Bytes(hex::decode(
                    "d9627aa40000000000000000000000000000000000000000000000000000000000000080000000000000000000000000000000000000000000000000016345785d8a00000000000000000000000000000000000000000000000000001206e6c0056936e100000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20000000000000000000000006810e776880c02933d47db1b9fc05908e5386b96869584cd0000000000000000000000001000000000000000000000000000000000000011000000000000000000000000000000000000000000000092415e982f60d431ba"
                ).unwrap()),
                value: U256::from_dec_str("0").unwrap(),
            })
        });

        // On first invocation no prior allowance, then max allowance set.
        let mut seq = Sequence::new();
        allowance_fetcher
            .expect_get_approval()
            .times(1)
            .with(eq(sell_token), eq(allowance_target), eq(U256::from(100)))
            .returning(move |_, _, _| {
                Ok(Approval::Approve {
                    token: sell_token,
                    spender: allowance_target,
                })
            })
            .in_sequence(&mut seq);
        allowance_fetcher
            .expect_get_approval()
            .times(1)
            .returning(|_, _, _| Ok(Approval::AllowanceSufficient))
            .in_sequence(&mut seq);

        let solver = MatchaSolver {
            client,
            allowance_fetcher,
        };

        let order = LimitOrder {
            sell_token,
            buy_token,
            sell_amount: 100.into(),
            buy_amount: 90.into(),
            ..Default::default()
        };

        // On first run we have two main interactions (approve + swap)
        let result = solver.settle_order(order.clone()).await.unwrap().unwrap();
        assert_eq!(result.encoder.finish().interactions[1].len(), 2);

        // On second run we have only have one main interactions (swap)
        let result = solver.settle_order(order).await.unwrap().unwrap();
        assert_eq!(result.encoder.finish().interactions[1].len(), 1)
    }
}
