//! Module for interacting with the Balancer SOR HTTP API.
//! https://dev.balancer.fi/resources/smart-order-router
use anyhow::Result;
use ethcontract::{H160, H256, U256};
use model::order::OrderKind;
use model::u256_decimal;
use reqwest::{Client, IntoUrl, Url};
use serde::{Deserialize, Serialize};
use web3::types::Bytes;

/// Balancer SOR API.
pub struct BalancerSorApi {
    client: Client,
    url: Url,
}

impl BalancerSorApi {
    /// Creates a new Balancer SOR API instance.
    pub fn new(client: Client, base_url: impl IntoUrl, chain_id: u64) -> Result<Self> {
        let url = base_url.into_url()?.join(&chain_id.to_string())?;
        Ok(Self { client, url })
    }

    /// Quotes a price.
    pub async fn quote(&self, query: Query) -> Result<Quote> {
        tracing::debug!(url =% self.url, ?query, "querying Balancer SOR");
        let response = self
            .client
            .post(self.url.clone())
            .json(&query)
            .send()
            .await?
            .text()
            .await?;
        tracing::debug!(%response, "received Balancer SOR quote");

        Ok(serde_json::from_str(&response)?)
    }
}

/// An SOR query.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Query {
    /// The sell token to quote.
    pub sell_token: H160,
    /// The buy token to quote.
    pub buy_token: H160,
    /// The order kind to use.
    pub order_kind: OrderKind,
    /// The amount to quote
    ///
    /// For sell orders this is the exact amount of sell token to trade, for buy
    /// orders, this is the amount of buy tokens to buy.
    #[serde(with = "u256_decimal")]
    pub amount: U256,
    /// The current gas price estimate used for determining how the trading
    /// route should be split.
    #[serde(with = "u256_decimal")]
    pub gas_price: U256,
}

/// The swap route found by the Balancer SOR service.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Quote {
    /// The swapped sell token amount.
    #[serde(with = "serde_u256_wrapped")]
    pub swap_amount: U256,
    /// The received buy token amount.
    #[serde(with = "serde_u256_wrapped")]
    pub return_amount: U256,
    /// The received considering fees.
    #[serde(with = "serde_u256_wrapped")]
    pub return_amount_considering_fees: U256,
    /// The swap route.
    pub swaps: Vec<Swap>,
    /// The token addresses included in the swap route.
    pub token_addresses: Vec<H160>,
    /// The input (sell) token.
    pub token_in: H160,
    /// The output (buy) token.
    pub token_out: H160,
    /// The price impact (i.e. market slippage).
    #[serde(with = "serde_with::rust::display_fromstr")]
    pub market_sp: f64,
    /// The swapped sell amount to use for encoding the Balancer batch swap.
    #[serde(with = "serde_u256_wrapped")]
    pub swap_amount_for_swaps: U256,
    /// The received buy amount to use for encoding the Balancer batch swap.
    #[serde(with = "serde_u256_wrapped")]
    pub return_amount_from_swaps: U256,
}

/// A swap included in a larger batched swap.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Swap {
    /// The ID of the pool swapping in this step.
    pub pool_id: H256,
    /// The index in `token_addresses` for the input token.
    pub asset_in_index: usize,
    /// The index in `token_addresses` for the ouput token.
    pub asset_out_index: usize,
    /// The amount to swap.
    #[serde(with = "u256_decimal")]
    pub amount: U256,
    /// Additional user data to pass to the pool.
    pub user_data: Bytes,
}

/// Module for deserializing the "wrapped" big integer types in the Balancer SOR
/// API response (of the form `{ "type": "BigNumber", hex: "0x..." }`).
mod serde_u256_wrapped {
    use ethcontract::U256;
    use serde::{de, Deserialize, Deserializer};

    #[derive(Deserialize)]
    struct Wrapped {
        #[serde(rename = "type")]
        kind: String,
        hex: U256,
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<U256, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wrapped = Wrapped::deserialize(deserializer)?;
        if wrapped.kind != "BigNumber" {
            return Err(de::Error::custom(format!(
                "unexpected big number type {}",
                wrapped.kind
            )));
        }

        Ok(wrapped.hex)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex_literal::hex;
    use serde_json::json;
    use std::env;

    #[test]
    fn serialize_query() {
        assert_eq!(
            serde_json::to_value(&Query {
                sell_token: addr!("ba100000625a3754423978a60c9317c58a424e3d"),
                buy_token: addr!("6b175474e89094c44da98b954eedeac495271d0f"),
                order_kind: OrderKind::Sell,
                amount: 1_000_000_000_000_000_000_u128.into(),
                gas_price: 10_000_000.into(),
            })
            .unwrap(),
            json!({
                "sellToken": "0xba100000625a3754423978a60c9317c58a424e3d",
                "buyToken": "0x6b175474e89094c44da98b954eedeac495271d0f",
                "orderKind": "sell",
                "amount": "1000000000000000000",
                "gasPrice": "10000000",
            }),
        );
    }

    #[test]
    #[allow(clippy::excessive_precision)]
    fn deserialize_quote() {
        assert_eq!(
            serde_json::from_value::<Quote>(json!({
                "swapAmount":{
                    "type": "BigNumber",
                    "hex": "0x0de0b6b3a7640000",
                },
                "returnAmount":{
                    "type": "BigNumber",
                    "hex": "0xc7dda274dffbd34e",
                },
                "returnAmountConsideringFees":{
                    "type": "BigNumber",
                    "hex": "0xc7d43370ffc29870",
                },
                "swaps":[
                    {
                        "poolId": "0x9c08c7a7a89cfd671c79eacdc6f07c1996277ed5000200000000000000000025",
                        "assetInIndex":0,
                        "assetOutIndex":1,
                        "amount": "1000000000000000000",
                        "userData": "0x",
                    },
                    {
                        "poolId": "0x06df3b2bbb68adc8b0e302443692037ed9f91b42000000000000000000000063",
                        "assetInIndex":1,
                        "assetOutIndex":2,
                        "amount": "0",
                        "userData": "0x",
                    },
                ],
                "tokenAddresses":[
                    "0xba100000625a3754423978a60c9317c58a424e3d",
                    "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
                    "0x6b175474e89094c44da98b954eedeac495271d0f",
                ],
                "tokenIn": "0xba100000625a3754423978a60c9317c58a424e3d",
                "tokenOut": "0x6b175474e89094c44da98b954eedeac495271d0f",
                "marketSp": "0.06920157586731092586977924035977274",
                "swapAmountForSwaps":{
                    "type": "BigNumber",
                    "hex": "0x0de0b6b3a7640000",
                },
                "returnAmountFromSwaps":{
                    "type": "BigNumber",
                    "hex": "0xc7dda274dffbd34e",
                },
            })).unwrap(),
            Quote {
                swap_amount: 1_000_000_000_000_000_000_u128.into(),
                return_amount: 14_401_845_806_258_443_086_u128.into(),
                return_amount_considering_fees: 14_399_190_469_030_615_152_u128.into(),
                swaps: vec![
                    Swap {
                        pool_id: H256(hex!("9c08c7a7a89cfd671c79eacdc6f07c1996277ed5000200000000000000000025")),
                        asset_in_index: 0,
                        asset_out_index: 1,
                        amount: 1_000_000_000_000_000_000_u128.into(),
                        user_data: Default::default(),
                    },
                    Swap {
                        pool_id: H256(hex!("06df3b2bbb68adc8b0e302443692037ed9f91b42000000000000000000000063")),
                        asset_in_index: 1,
                        asset_out_index: 2,
                        amount: 0.into(),
                        user_data: Default::default(),
                    },
                ],
                token_addresses: vec![
                    addr!("ba100000625a3754423978a60c9317c58a424e3d"),
                    addr!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
                    addr!("6b175474e89094c44da98b954eedeac495271d0f"),
                ],
                token_in: addr!("ba100000625a3754423978a60c9317c58a424e3d"),
                token_out: addr!("6b175474e89094c44da98b954eedeac495271d0f"),
                market_sp: 0.06920157586731092586977924035977274,
                swap_amount_for_swaps: 1_000_000_000_000_000_000_u128.into(),
                return_amount_from_swaps: 14_401_845_806_258_443_086_u128.into(),
            },
        );
    }

    #[tokio::test]
    #[ignore]
    async fn balancer_sor_quote() {
        let url = env::var("BALANCER_SOR_URL").unwrap();
        let api = BalancerSorApi::new(Client::new(), url, 1).unwrap();

        fn base(atoms: U256) -> String {
            let base = atoms.to_f64_lossy() / 1e18;
            format!("{:.6}", base)
        }

        let sell_quote = api
            .quote(Query {
                sell_token: addr!("ba100000625a3754423978a60c9317c58a424e3d"),
                buy_token: addr!("6b175474e89094c44da98b954eedeac495271d0f"),
                order_kind: OrderKind::Sell,
                amount: 1_000_000_000_000_000_000_u128.into(),
                gas_price: 10_000_000.into(),
            })
            .await
            .unwrap();
        println!("Sell 1.0 BAL for {:.4} DAI", base(sell_quote.return_amount));

        let buy_quote = api
            .quote(Query {
                sell_token: addr!("ba100000625a3754423978a60c9317c58a424e3d"),
                buy_token: addr!("6b175474e89094c44da98b954eedeac495271d0f"),
                order_kind: OrderKind::Buy,
                amount: 100_000_000_000_000_000_000_u128.into(),
                gas_price: 10_000_000.into(),
            })
            .await
            .unwrap();
        println!("Buy {:.4} BAL for 100.0 DAI", base(buy_quote.return_amount));
    }
}
