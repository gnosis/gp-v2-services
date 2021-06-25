//! Module containing implementation of the Matcha solver.
//!
//! This solver will simply use the Matcha API to get a quote for a
//! single GPv2 order and produce a settlement directly against Matcha.

pub mod api;

use super::solver_utils::{AllowanceFetching, Slippage};
use crate::solver::matcha_solver::api::MatchaApi;
use anyhow::{ensure, Result};
use contracts::{GPv2Settlement, ERC20};
use ethcontract::{dyns::DynWeb3, Bytes, U256};
use maplit::hashmap;

use super::single_order_solver::SingleOrderSolving;

use self::api::{DefaultMatchaApi, SwapQuery, SwapResponse};
use crate::{
    encoding::EncodedInteraction,
    interactions::Erc20ApproveInteraction,
    liquidity::LimitOrder,
    settlement::{Interaction, Settlement},
};
use model::order::OrderKind;
use std::fmt::{self, Display, Formatter};

/// Constant maximum slippage of 5 BPS (0.05%) to use for on-chain liquidity.
/// This is half the 1inch slippage
pub const STANDARD_MATCHA_SLIPPAGE_BPS: u16 = 5;

/// A GPv2 solver that matches GP orders to direct Matcha swaps.
pub struct MatchaSolver<F> {
    settlement_contract: GPv2Settlement,
    client: Box<dyn MatchaApi + Send + Sync>,
    allowance_fetcher: F,
}

/// Chain ID for Mainnet.
const MAINNET_CHAIN_ID: u64 = 1;

impl MatchaSolver<GPv2Settlement> {
    pub fn new(settlement_contract: GPv2Settlement, chain_id: u64) -> Result<Self> {
        ensure!(
            chain_id == MAINNET_CHAIN_ID,
            "Matcha solver only supported on Mainnet",
        );

        let allowance_fetcher = settlement_contract.clone();
        Ok(Self {
            settlement_contract,
            allowance_fetcher,
            client: Box::new(DefaultMatchaApi::default()),
        })
    }
}
impl<F> MatchaSolver<F> {
    fn web3(&self) -> DynWeb3 {
        self.settlement_contract.raw_instance().web3()
    }
}

impl<F> std::fmt::Debug for MatchaSolver<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MatchaSolver")
    }
}

#[async_trait::async_trait]
impl<F: AllowanceFetching> SingleOrderSolving for MatchaSolver<F> {
    async fn settle_order(&self, order: LimitOrder) -> Result<Option<Settlement>> {
        let (swap, mut settlement) = match order.kind {
            OrderKind::Sell => {
                let query = SwapQuery {
                    sell_token: order.sell_token,
                    buy_token: order.buy_token,
                    sell_amount: Some(order.sell_amount),
                    buy_amount: None,
                    slippage_percentage: Slippage::basis_points(STANDARD_MATCHA_SLIPPAGE_BPS)
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
                let settlement = Settlement::new(hashmap! {
                    order.sell_token => swap.sell_amount,
                    order.buy_token => swap.buy_amount,
                });
                (swap, settlement)
            }
            OrderKind::Buy => {
                let query = SwapQuery {
                    sell_token: order.sell_token,
                    buy_token: order.buy_token,
                    sell_amount: None,
                    buy_amount: Some(order.buy_amount),
                    slippage_percentage: Slippage::basis_points(STANDARD_MATCHA_SLIPPAGE_BPS)
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
                let settlement = Settlement::new(hashmap! {
                    order.sell_token => swap.sell_amount,
                    order.buy_token => swap.buy_amount,
                });
                (swap, settlement)
            }
        };
        let spender = swap.allowance_target;
        let sell_token = ERC20::at(&self.web3(), order.sell_token);
        let existing_allowance = self
            .allowance_fetcher
            .existing_allowance(order.sell_token, spender)
            .await?;

        settlement.with_liquidity(&order, swap.sell_amount)?;

        if existing_allowance < swap.sell_amount {
            settlement
                .encoder
                .append_to_execution_plan(Erc20ApproveInteraction {
                    token: sell_token,
                    spender,
                    amount: U256::MAX,
                });
        }
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

impl<F> Display for MatchaSolver<F> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "MatchaSolver")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::liquidity::LimitOrder;
    use crate::solver::matcha_solver::api::MockMatchaApi;
    use crate::solver::solver_utils::MockAllowanceFetching;
    use contracts::{GPv2Settlement, WETH9};
    use ethcontract::{Web3, H160};
    use mockall::Sequence;
    use model::order::{Order, OrderCreation, OrderKind};
    use shared::dummy_contract;
    use shared::transport::{create_env_test_transport, create_test_transport, dummy};

    #[tokio::test]
    #[ignore]
    async fn solve_sell_order_on_matcha() {
        let web3 = Web3::new(create_env_test_transport());
        let chain_id = web3.eth().chain_id().await.unwrap().as_u64();
        let settlement = GPv2Settlement::deployed(&web3).await.unwrap();

        let weth = WETH9::deployed(&web3).await.unwrap();
        let gno = shared::addr!("6810e776880c02933d47db1b9fc05908e5386b96");

        let solver = MatchaSolver::new(settlement, chain_id).unwrap();
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

        let solver = MatchaSolver::new(settlement, chain_id).unwrap();
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
        let mut allowance_fetcher = MockAllowanceFetching::new();

        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(1);

        client.expect_get_swap().returning(|_| {
            Ok(SwapResponse {
            sell_amount: U256::from_dec_str("100").unwrap(),
             buy_amount: U256::from_dec_str("91").unwrap(),
             allowance_target: shared::addr!("def1c0ded9bec7f1a1670819833240f027b25eff"),
            price: 0.91_f64,
            to: shared::addr!("def1c0ded9bec7f1a1670819833240f027b25eff"),
            data: web3::types::Bytes(hex::decode(
                "d9627aa40000000000000000000000000000000000000000000000000000000000000080000000000000000000000000000000000000000000000000016345785d8a00000000000000000000000000000000000000000000000000001206e6c0056936e100000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20000000000000000000000006810e776880c02933d47db1b9fc05908e5386b96869584cd0000000000000000000000001000000000000000000000000000000000000011000000000000000000000000000000000000000000000092415e982f60d431ba"
            ).unwrap()),
            value: U256::from_dec_str("0").unwrap(),
        })});

        allowance_fetcher
            .expect_existing_allowance()
            .returning(|_, _| Ok(U256::zero()));

        let solver = MatchaSolver {
            settlement_contract: GPv2Settlement::at(&dummy::web3(), H160::zero()),
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
                sell_token => 99.into(),
                buy_token => 91.into(),
            }
        );

        let result = solver.settle_order(order_violating_limit).await.unwrap();
        assert!(result.is_none());
    }
    #[tokio::test]
    async fn test_satisfies_limit_price_for_buy_order() {
        let mut client = Box::new(MockMatchaApi::new());
        let mut allowance_fetcher = MockAllowanceFetching::new();

        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(1);

        client.expect_get_swap().returning(|_| {
            Ok(SwapResponse {
            sell_amount: U256::from_dec_str("100").unwrap(),
             buy_amount: U256::from_dec_str("91").unwrap(),
             allowance_target: shared::addr!("def1c0ded9bec7f1a1670819833240f027b25eff"),
            price: 0.91_f64,
            to: shared::addr!("def1c0ded9bec7f1a1670819833240f027b25eff"),
            data: web3::types::Bytes(hex::decode(
                "d9627aa40000000000000000000000000000000000000000000000000000000000000080000000000000000000000000000000000000000000000000016345785d8a00000000000000000000000000000000000000000000000000001206e6c0056936e100000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20000000000000000000000006810e776880c02933d47db1b9fc05908e5386b96869584cd0000000000000000000000001000000000000000000000000000000000000011000000000000000000000000000000000000000000000092415e982f60d431ba"
            ).unwrap()),
            value: U256::from_dec_str("0").unwrap(),
        })});

        allowance_fetcher
            .expect_existing_allowance()
            .returning(|_, _| Ok(U256::zero()));

        let solver = MatchaSolver {
            settlement_contract: GPv2Settlement::at(&dummy::web3(), H160::zero()),
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
                sell_token => 99.into(),
                buy_token => 91.into(),
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

        assert!(MatchaSolver::new(settlement, chain_id).is_err())
    }

    #[tokio::test]
    async fn test_sets_allowance_if_necessary() {
        let mut client = Box::new(MockMatchaApi::new());
        let mut allowance_fetcher = MockAllowanceFetching::new();

        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(1);

        client.expect_get_swap().returning(|_| {
            Ok(SwapResponse {
                sell_amount: U256::from_dec_str("100").unwrap(),
                 buy_amount: U256::from_dec_str("91").unwrap(),
                 allowance_target: shared::addr!("def1c0ded9bec7f1a1670819833240f027b25eff"),
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
            .expect_existing_allowance()
            .times(1)
            .returning(|_, _| Ok(U256::zero()))
            .in_sequence(&mut seq);
        allowance_fetcher
            .expect_existing_allowance()
            .times(1)
            .returning(|_, _| Ok(U256::max_value()))
            .in_sequence(&mut seq);

        let solver = MatchaSolver {
            client,
            allowance_fetcher,
            settlement_contract: dummy_contract!(GPv2Settlement, H160::zero()),
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
