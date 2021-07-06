mod api;

use self::api::{
    DefaultParaswapApi, ParaswapApi, PriceQuery, PriceResponse, Side, TransactionBuilderQuery,
    TransactionBuilderResponse,
};
use super::single_order_solver::SingleOrderSolving;
use crate::{
    encoding::EncodedInteraction,
    interactions::allowances::{AllowanceManager, AllowanceManaging},
    liquidity::LimitOrder,
    settlement::{Interaction, Settlement},
};
use anyhow::{anyhow, Result};
use contracts::GPv2Settlement;
use derivative::Derivative;
use ethcontract::{Bytes, H160};
use maplit::hashmap;
use shared::{conversions::U256Ext, token_info::TokenInfoFetching, Web3};
use std::sync::Arc;

const REFERRER: &str = "GPv2";
const APPROVAL_RECEIVER: H160 = shared::addr!("b70bc06d2c9bf03b3373799606dc7d39346c06b3");

/// A GPv2 solver that matches GP orders to direct ParaSwap swaps.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct ParaswapSolver {
    settlement_contract: GPv2Settlement,
    solver_address: H160,
    #[derivative(Debug = "ignore")]
    token_info: Arc<dyn TokenInfoFetching>,
    #[derivative(Debug = "ignore")]
    allowance_fetcher: Box<dyn AllowanceManaging>,
    #[derivative(Debug = "ignore")]
    client: Box<dyn ParaswapApi + Send + Sync>,
    slippage_bps: usize,
}

impl ParaswapSolver {
    pub fn new(
        web3: Web3,
        settlement_contract: GPv2Settlement,
        solver_address: H160,
        token_info: Arc<dyn TokenInfoFetching>,
        slippage_bps: usize,
    ) -> Self {
        let allowance_fetcher = AllowanceManager::new(web3, settlement_contract.address());
        Self {
            settlement_contract,
            solver_address,
            token_info,
            allowance_fetcher: Box::new(allowance_fetcher),
            client: Box::new(DefaultParaswapApi::default()),
            slippage_bps,
        }
    }
}

#[async_trait::async_trait]
impl SingleOrderSolving for ParaswapSolver {
    async fn settle_order(&self, order: LimitOrder) -> Result<Option<Settlement>> {
        let (amount, side) = match order.kind {
            model::order::OrderKind::Buy => (order.buy_amount, Side::Buy),
            model::order::OrderKind::Sell => (order.sell_amount, Side::Sell),
        };
        let token_infos = self
            .token_info
            .get_token_infos(&[order.sell_token, order.buy_token])
            .await;
        let decimals = |token: &H160| {
            token_infos
                .get(token)
                .and_then(|info| info.decimals.map(usize::from))
                .ok_or_else(|| anyhow!("decimals for token {:?} not found", token))
        };

        let price_query = PriceQuery {
            from: order.sell_token,
            to: order.buy_token,
            from_decimals: decimals(&order.sell_token)?,
            to_decimals: decimals(&order.buy_token)?,
            amount,
            side,
        };

        let price_response = self.client.price(price_query).await?;

        if !satisfies_limit_price(&order, &price_response) {
            tracing::debug!("Order limit price not respected");
            return Ok(None);
        }

        let (src_amount, dest_amount) = match order.kind {
            // Buy orders apply slippage to src amount, dest amount unchanged
            model::order::OrderKind::Buy => (
                price_response
                    .src_amount
                    .checked_mul((10000 + self.slippage_bps).into())
                    .ok_or_else(|| anyhow!("Overflow during slippage computation"))?
                    / 10000,
                price_response.dest_amount,
            ),
            // Sell orders apply slippage to dest amount, src amount unchanged
            model::order::OrderKind::Sell => (
                price_response.src_amount,
                price_response
                    .dest_amount
                    .checked_mul((10000 - self.slippage_bps).into())
                    .ok_or_else(|| anyhow!("Overflow during slippage computation"))?
                    / 10000,
            ),
        };
        let transaction_query = TransactionBuilderQuery {
            src_token: order.sell_token,
            dest_token: order.buy_token,
            src_amount,
            dest_amount,
            from_decimals: decimals(&order.sell_token)?,
            to_decimals: decimals(&order.buy_token)?,
            price_route: price_response.price_route_raw,
            user_address: self.solver_address,
            referrer: REFERRER.to_string(),
        };
        let transaction = self.client.transaction(transaction_query).await?;

        let mut settlement = Settlement::new(hashmap! {
            order.sell_token => price_response.dest_amount,
            order.buy_token => price_response.src_amount,
        });
        settlement.with_liquidity(&order, amount)?;

        settlement.encoder.append_to_execution_plan(
            self.allowance_fetcher
                .get_approval(
                    order.sell_token,
                    APPROVAL_RECEIVER,
                    price_response.src_amount,
                )
                .await?,
        );
        settlement.encoder.append_to_execution_plan(transaction);
        Ok(Some(settlement))
    }

    fn name(&self) -> &'static str {
        "ParaSwap"
    }
}

impl Interaction for TransactionBuilderResponse {
    fn encode(&self) -> Vec<EncodedInteraction> {
        vec![(self.to, self.value, Bytes(self.data.0.clone()))]
    }
}

fn satisfies_limit_price(order: &LimitOrder, response: &PriceResponse) -> bool {
    // We check if order.sell / order.buy >= response.sell / response.buy
    order.sell_amount.to_big_rational() * response.dest_amount.to_big_rational()
        >= response.src_amount.to_big_rational() * order.buy_amount.to_big_rational()
}

#[cfg(test)]
mod tests {
    use super::{api::MockParaswapApi, *};
    use crate::interactions::allowances::{Approval, MockAllowanceManaging};
    use contracts::WETH9;
    use ethcontract::U256;
    use mockall::predicate::*;
    use mockall::Sequence;
    use model::order::{Order, OrderCreation, OrderKind};
    use shared::{
        dummy_contract,
        token_info::{MockTokenInfoFetching, TokenInfo, TokenInfoFetcher},
        transport::create_env_test_transport,
    };
    use std::collections::HashMap;
    use hex_literal::hex;
    use serde_json::json;

    #[test]
    fn parse_test() {
        let s = TransactionBuilderQuery {
            src_token: H160::from_slice(&hex!("a47c8bf37f92abed4a126bda807a7b7498661acd")),
            dest_token: H160::from_slice(&hex!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2")),
            src_amount: U256::from_dec_str("1994169551053737031680").unwrap(),
            dest_amount: U256::from_dec_str("897298442321218920").unwrap(),
            from_decimals: 18,
            to_decimals: 18,
            price_route: json!(
             {
              "adapterVersion": "4.0.0",
              "bestRoute": [
                {
                  "destAmount": "898196638960179100",
                  "destAmountFeeDeducted": "898196638960179100",
                  "exchange": "MultiPath",
                  "percent": "100",
                  "srcAmount": "1994169551053737031680"
                }
              ],
              "bestRouteGas": "296300",
              "bestRouteGasCostUSD": "3.966378",
              "blockNumber": 12771136,
              "contractMethod": "multiSwap",
              "destAmount": "898196638960179100",
              "destAmountFeeDeducted": "898196638960179100",
              "details": {
                "destAmount": "898196638960179100",
                "srcAmount": "1994169551053737031680",
                "tokenFrom": "0xa47c8bf37f92abed4a126bda807a7b7498661acd",
                "tokenTo": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
              },
              "fromUSD": "1994.1695510537",
              "hmac": "a232c77298f8a6de61820243fc5875d37ab7a2c7",
              "maxImpactReached": false,
              "multiPath": true,
              "multiRoute": [
                [
                  {
                    "data": {
                      "factory": "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
                      "gasUSD": "1.070908",
                      "initCode": "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
                      "path": [
                        "0xa47c8bf37f92abed4a126bda807a7b7498661acd",
                        "0xdac17f958d2ee523a2206206994597c13d831ec7"
                      ],
                      "router": "0x86d3579b043585A97532514016dCF0C2d6C4b6a1",
                      "tokenFrom": "0xa47c8bf37f92abed4a126bda807a7b7498661acd",
                      "tokenTo": "0xdac17f958d2ee523a2206206994597c13d831ec7"
                    },
                    "destAmount": "1995508094",
                    "destAmountFeeDeducted": "1995508094",
                    "exchange": "UniswapV2",
                    "percent": "100",
                    "srcAmount": "1994169551053737031680"
                  }
                ],
                [
                  {
                    "data": {
                      "gasUSD": "1.338636",
                      "tokenFrom": "0xdac17f958d2ee523a2206206994597c13d831ec7",
                      "tokenTo": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
                      "version": 4,
                    },
                    "destAmount": "898196638960179100",
                    "destAmountFeeDeducted": "898196638960179100",
                    "exchange": "ParaSwapPool3",
                    "percent": "100",
                    "srcAmount": "1995508094"
                  }
                ]
              ],
              "others": [
                {
                  "data": {
                    "factory": "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
                    "gasUSD": "1.070908",
                    "initCode": "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
                    "path": [
                      "0xa47c8bf37f92abed4a126bda807a7b7498661acd",
                      "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                    ],
                    "router": "0x86d3579b043585A97532514016dCF0C2d6C4b6a1",
                    "tokenFrom": "0xa47c8bf37f92abed4a126bda807a7b7498661acd",
                    "tokenTo": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                  },
                  "exchange": "UniswapV2",
                  "rate": "258687941544023809",
                  "rateFeeDeducted": "258687941544023809",
                  "unit": "476946191867525",
                  "unitFeeDeducted": "476946191867525"
                },
                {
                  "data": {
                    "factory": "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
                    "gasUSD": "1.204772",
                    "initCode": "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
                    "path": [
                      "0xa47c8bf37f92abed4a126bda807a7b7498661acd",
                      "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                    ],
                    "router": "0xBc1315CD2671BC498fDAb42aE1214068003DC51e",
                    "tokenFrom": "0xa47c8bf37f92abed4a126bda807a7b7498661acd",
                    "tokenTo": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                  },
                  "exchange": "SushiSwap",
                  "rate": "889178407612251070",
                  "rateFeeDeducted": "889178407612251070",
                  "unit": "447094570010879",
                  "unitFeeDeducted": "447094570010879"
                },
                {
                  "exchange": "MultiPath",
                  "rate": "898196638960179100",
                  "rateFeeDeducted": "898196638960179100",
                  "unit": "-",
                  "unitFeeDeducted": "-"
                }
              ],
              "priceID": "7fab2c88-cff9-4507-bb79-1278770c589a",
              "priceWithSlippage": "889214672570577309",
              "side": "SELL",
              "spender": "0xb70Bc06D2c9Bf03b3373799606dc7d39346c06B3",
              "srcAmount": "1994169551053737031680",
              "toUSD": "2007.3527225129",
              "toUSDFeeDeducted": "2007.3527225129"
            }
            ),
            user_address: H160::from_slice(&hex!("a6ddbd0de6b310819b49f680f65871bee85f517e")),
            referrer: "GPv2".parse().unwrap()
        };
        let log_string = "TransactionBuilderQuery { src_token: 0xa47c8bf37f92abed4a126bda807a7b7498661acd, dest_token: 0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2, src_amount: 1994169551053737031680, dest_amount: 897298442321218920, from_decimals: 18, to_decimals: 18, price_route: Object({\"adapterVersion\": String(\"4.0.0\"), \"bestRoute\": Array([Object({\"destAmount\": String(\"898196638960179100\"), \"destAmountFeeDeducted\": String(\"898196638960179100\"), \"exchange\": String(\"MultiPath\"), \"percent\": String(\"100\"), \"srcAmount\": String(\"1994169551053737031680\")})]), \"bestRouteGas\": String(\"296300\"), \"bestRouteGasCostUSD\": String(\"3.966378\"), \"blockNumber\": Number(12771136), \"contractMethod\": String(\"multiSwap\"), \"destAmount\": String(\"898196638960179100\"), \"destAmountFeeDeducted\": String(\"898196638960179100\"), \"details\": Object({\"destAmount\": String(\"898196638960179100\"), \"srcAmount\": String(\"1994169551053737031680\"), \"tokenFrom\": String(\"0xa47c8bf37f92abed4a126bda807a7b7498661acd\"), \"tokenTo\": String(\"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2\")}), \"fromUSD\": String(\"1994.1695510537\"), \"hmac\": String(\"a232c77298f8a6de61820243fc5875d37ab7a2c7\"), \"maxImpactReached\": Bool(false), \"multiPath\": Bool(true), \"multiRoute\": Array([Array([Object({\"data\": Object({\"factory\": String(\"0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f\"), \"gasUSD\": String(\"1.070908\"), \"initCode\": String(\"0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f\"), \"path\": Array([String(\"0xa47c8bf37f92abed4a126bda807a7b7498661acd\"), String(\"0xdac17f958d2ee523a2206206994597c13d831ec7\")]), \"router\": String(\"0x86d3579b043585A97532514016dCF0C2d6C4b6a1\"), \"tokenFrom\": String(\"0xa47c8bf37f92abed4a126bda807a7b7498661acd\"), \"tokenTo\": String(\"0xdac17f958d2ee523a2206206994597c13d831ec7\")}), \"destAmount\": String(\"1995508094\"), \"destAmountFeeDeducted\": String(\"1995508094\"), \"exchange\": String(\"UniswapV2\"), \"percent\": String(\"100\"), \"srcAmount\": String(\"1994169551053737031680\")})]), Array([Object({\"data\": Object({\"gasUSD\": String(\"1.338636\"), \"tokenFrom\": String(\"0xdac17f958d2ee523a2206206994597c13d831ec7\"), \"tokenTo\": String(\"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2\"), \"version\": Number(4)}), \"destAmount\": String(\"898196638960179100\"), \"destAmountFeeDeducted\": String(\"898196638960179100\"), \"exchange\": String(\"ParaSwapPool3\"), \"percent\": String(\"100\"), \"srcAmount\": String(\"1995508094\")})])]), \"others\": Array([Object({\"data\": Object({\"factory\": String(\"0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f\"), \"gasUSD\": String(\"1.070908\"), \"initCode\": String(\"0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f\"), \"path\": Array([String(\"0xa47c8bf37f92abed4a126bda807a7b7498661acd\"), String(\"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2\")]), \"router\": String(\"0x86d3579b043585A97532514016dCF0C2d6C4b6a1\"), \"tokenFrom\": String(\"0xa47c8bf37f92abed4a126bda807a7b7498661acd\"), \"tokenTo\": String(\"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2\")}), \"exchange\": String(\"UniswapV2\"), \"rate\": String(\"258687941544023809\"), \"rateFeeDeducted\": String(\"258687941544023809\"), \"unit\": String(\"476946191867525\"), \"unitFeeDeducted\": String(\"476946191867525\")}), Object({\"data\": Object({\"factory\": String(\"0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac\"), \"gasUSD\": String(\"1.204772\"), \"initCode\": String(\"0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303\"), \"path\": Array([String(\"0xa47c8bf37f92abed4a126bda807a7b7498661acd\"), String(\"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2\")]), \"router\": String(\"0xBc1315CD2671BC498fDAb42aE1214068003DC51e\"), \"tokenFrom\": String(\"0xa47c8bf37f92abed4a126bda807a7b7498661acd\"), \"tokenTo\": String(\"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2\")}), \"exchange\": String(\"SushiSwap\"), \"rate\": String(\"889178407612251070\"), \"rateFeeDeducted\": String(\"889178407612251070\"), \"unit\": String(\"447094570010879\"), \"unitFeeDeducted\": String(\"447094570010879\")}), Object({\"exchange\": String(\"MultiPath\"), \"rate\": String(\"898196638960179100\"), \"rateFeeDeducted\": String(\"898196638960179100\"), \"unit\": String(\"-\"), \"unitFeeDeducted\": String(\"-\")})]), \"priceID\": String(\"7fab2c88-cff9-4507-bb79-1278770c589a\"), \"priceWithSlippage\": String(\"889214672570577309\"), \"side\": String(\"SELL\"), \"spender\": String(\"0xb70Bc06D2c9Bf03b3373799606dc7d39346c06B3\"), \"srcAmount\": String(\"1994169551053737031680\"), \"toUSD\": String(\"2007.3527225129\"), \"toUSDFeeDeducted\": String(\"2007.3527225129\")}), user_address: 0xa6ddbd0de6b310819b49f680f65871bee85f517e, referrer: \"GPv2\" }";
        assert_eq!(format!("{:?}", s), log_string);
        println!("{}", serde_json::to_string_pretty(&s).unwrap());
    }

    #[test]
    fn test_satisfies_limit_price() {
        assert!(!satisfies_limit_price(
            &LimitOrder {
                sell_amount: 100.into(),
                buy_amount: 95.into(),
                ..Default::default()
            },
            &PriceResponse {
                src_amount: 100.into(),
                dest_amount: 90.into(),
                ..Default::default()
            }
        ));

        assert!(satisfies_limit_price(
            &LimitOrder {
                sell_amount: 100.into(),
                buy_amount: 95.into(),
                ..Default::default()
            },
            &PriceResponse {
                src_amount: 100.into(),
                dest_amount: 100.into(),
                ..Default::default()
            }
        ));

        assert!(satisfies_limit_price(
            &LimitOrder {
                sell_amount: 100.into(),
                buy_amount: 95.into(),
                ..Default::default()
            },
            &PriceResponse {
                src_amount: 100.into(),
                dest_amount: 95.into(),
                ..Default::default()
            }
        ));
    }

    #[tokio::test]
    async fn test_skips_order_if_unable_to_fetch_decimals() {
        let client = Box::new(MockParaswapApi::new());
        let allowance_fetcher = Box::new(MockAllowanceManaging::new());
        let mut token_info = MockTokenInfoFetching::new();

        token_info
            .expect_get_token_infos()
            .return_const(HashMap::new());

        let solver = ParaswapSolver {
            client,
            solver_address: Default::default(),
            token_info: Arc::new(token_info),
            allowance_fetcher,
            settlement_contract: dummy_contract!(GPv2Settlement, H160::zero()),
            slippage_bps: 10,
        };

        let order = LimitOrder::default();
        let result = solver.settle_order(order).await;

        // This implicitly checks that we don't call the API is its mock doesn't have any expectations and would panic
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_respects_limit_price() {
        let mut client = Box::new(MockParaswapApi::new());
        let mut allowance_fetcher = Box::new(MockAllowanceManaging::new());
        let mut token_info = MockTokenInfoFetching::new();

        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(2);

        client.expect_price().returning(|_| {
            Ok(PriceResponse {
                price_route_raw: Default::default(),
                src_amount: 100.into(),
                dest_amount: 99.into(),
            })
        });
        client
            .expect_transaction()
            .returning(|_| Ok(Default::default()));

        allowance_fetcher
            .expect_get_approval()
            .returning(|_, _, _| Ok(Approval::AllowanceSufficient));

        token_info.expect_get_token_infos().returning(move |_| {
            hashmap! {
                sell_token => TokenInfo { decimals: Some(18)},
                buy_token => TokenInfo { decimals: Some(18)},
            }
        });

        let solver = ParaswapSolver {
            client,
            solver_address: Default::default(),
            token_info: Arc::new(token_info),
            allowance_fetcher,
            settlement_contract: dummy_contract!(GPv2Settlement, H160::zero()),
            slippage_bps: 10,
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
                buy_token => 100.into(),
            }
        );

        let result = solver.settle_order(order_violating_limit).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_sets_allowance_if_necessary() {
        let mut client = Box::new(MockParaswapApi::new());
        let mut allowance_fetcher = Box::new(MockAllowanceManaging::new());
        let mut token_info = MockTokenInfoFetching::new();

        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(2);

        client.expect_price().returning(|_| {
            Ok(PriceResponse {
                price_route_raw: Default::default(),
                src_amount: 100.into(),
                dest_amount: 99.into(),
            })
        });
        client
            .expect_transaction()
            .returning(|_| Ok(Default::default()));

        // On first invocation no prior allowance, then max allowance set.
        let mut seq = Sequence::new();
        allowance_fetcher
            .expect_get_approval()
            .times(1)
            .with(eq(sell_token), eq(APPROVAL_RECEIVER), eq(U256::from(100)))
            .returning(move |_, _, _| {
                Ok(Approval::Approve {
                    token: sell_token,
                    spender: APPROVAL_RECEIVER,
                })
            })
            .in_sequence(&mut seq);
        allowance_fetcher
            .expect_get_approval()
            .times(1)
            .with(eq(sell_token), eq(APPROVAL_RECEIVER), eq(U256::from(100)))
            .returning(|_, _, _| Ok(Approval::AllowanceSufficient))
            .in_sequence(&mut seq);

        token_info.expect_get_token_infos().returning(move |_| {
            hashmap! {
                sell_token => TokenInfo { decimals: Some(18)},
                buy_token => TokenInfo { decimals: Some(18)},
            }
        });

        let solver = ParaswapSolver {
            client,
            solver_address: Default::default(),
            token_info: Arc::new(token_info),
            allowance_fetcher,
            settlement_contract: dummy_contract!(GPv2Settlement, H160::zero()),
            slippage_bps: 10,
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

    #[tokio::test]
    async fn test_sets_slippage() {
        let mut client = Box::new(MockParaswapApi::new());
        let mut allowance_fetcher = Box::new(MockAllowanceManaging::new());
        let mut token_info = MockTokenInfoFetching::new();

        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(2);

        client.expect_price().returning(|_| {
            Ok(PriceResponse {
                price_route_raw: Default::default(),
                src_amount: 100.into(),
                dest_amount: 99.into(),
            })
        });

        // Check slippage is applied to PriceResponse
        let mut seq = Sequence::new();
        client
            .expect_transaction()
            .times(1)
            .returning(|transaction| {
                assert_eq!(transaction.src_amount, 100.into());
                assert_eq!(transaction.dest_amount, 89.into());
                Ok(Default::default())
            })
            .in_sequence(&mut seq);
        client
            .expect_transaction()
            .times(1)
            .returning(|transaction| {
                assert_eq!(transaction.src_amount, 110.into());
                assert_eq!(transaction.dest_amount, 99.into());
                Ok(Default::default())
            })
            .in_sequence(&mut seq);

        allowance_fetcher
            .expect_get_approval()
            .returning(|_, _, _| Ok(Approval::AllowanceSufficient));

        token_info.expect_get_token_infos().returning(move |_| {
            hashmap! {
                sell_token => TokenInfo { decimals: Some(18)},
                buy_token => TokenInfo { decimals: Some(18)},
            }
        });

        let solver = ParaswapSolver {
            client,
            solver_address: Default::default(),
            token_info: Arc::new(token_info),
            allowance_fetcher,
            settlement_contract: dummy_contract!(GPv2Settlement, H160::zero()),
            slippage_bps: 1000, // 10%
        };

        let sell_order = LimitOrder {
            sell_token,
            buy_token,
            sell_amount: 100.into(),
            buy_amount: 90.into(),
            kind: model::order::OrderKind::Sell,
            ..Default::default()
        };

        let result = solver.settle_order(sell_order).await.unwrap();
        // Actual assertion is inside the client's `expect_transaction` mock
        assert!(result.is_some());

        let buy_order = LimitOrder {
            sell_token,
            buy_token,
            sell_amount: 100.into(),
            buy_amount: 90.into(),
            kind: model::order::OrderKind::Buy,
            ..Default::default()
        };
        let result = solver.settle_order(buy_order).await.unwrap();
        // Actual assertion is inside the client's `expect_transaction` mock
        assert!(result.is_some());
    }

    #[tokio::test]
    #[ignore]
    async fn solve_order_on_paraswap() {
        let web3 = Web3::new(create_env_test_transport());
        let settlement = GPv2Settlement::deployed(&web3).await.unwrap();
        // Pretend the settlement contract is solving for itself
        let solver = settlement.address();
        let token_info_fetcher = Arc::new(TokenInfoFetcher { web3: web3.clone() });

        let weth = WETH9::deployed(&web3).await.unwrap();
        let gno = shared::addr!("6810e776880c02933d47db1b9fc05908e5386b96");

        let solver = ParaswapSolver::new(web3, settlement, solver, token_info_fetcher, 0);

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
            .unwrap()
            .unwrap();

        println!("{:#?}", settlement);
    }
}
