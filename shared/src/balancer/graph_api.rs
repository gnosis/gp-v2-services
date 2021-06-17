//! Module containing The Graph API client used for retrieving Balancer weighted
//! pools from the Balancer V2 subgraph.
//!
//! The pools retrieved from this client are used to prime the graph event store
//! to reduce start-up time. We do not use this in general for retrieving pools
//! as to:
//! - not rely on external services
//! - ensure that we are using the latest up-to-date pool data by using events
//!   from the node

use super::pool_storage::RegisteredWeightedPool;
use crate::subgraph::SubgraphClient;
use anyhow::{bail, Result};
use serde_json::json;

/// The threshold of number of blocks after which its considered safe that there
/// will not be futher re-orgs
const REORG_THRESHOLD: u64 = 10;

/// The page size when querying pools.
#[cfg(not(test))]
const QUERY_PAGE_SIZE: usize = 1000;
#[cfg(test)]
const QUERY_PAGE_SIZE: usize = 10;

/// A client to the Balancer V2 subgraph.
///
/// This client is not implemented to allow general GraphQL queries, but instead
/// implements high-level methods that perform GraphQL queries under the hood.
pub struct BalancerSubgraphClient(SubgraphClient);

impl BalancerSubgraphClient {
    /// Creates a new Balancer subgraph client for the specified chain ID.
    pub fn for_chain(chain_id: u64) -> Result<Self> {
        let subgraph_name = match chain_id {
            1 => "balancer-v2",
            4 => "balancer-rinkeby-v2",
            _ => bail!("unsupported chain {}", chain_id),
        };
        Ok(Self(SubgraphClient::new("balancer-labs", subgraph_name)?))
    }

    /// Retrieves the list of registered pools from the subgraph.
    pub async fn get_weighted_pools(&self) -> Result<(u64, Vec<RegisteredWeightedPool>)> {
        use pools_query::*;

        let block_number = self.get_safe_block().await?;
        let mut results = Vec::<RegisteredWeightedPool>::new();
        while {
            // We do paging by last ID instead of using `skip`. This is the
            // suggested approach to paging best performance:
            // <https://thegraph.com/docs/graphql-api#pagination>
            let last_id = results
                .last()
                .map(|pool| json!(pool.pool_id))
                .unwrap_or_else(|| json!(""));

            let page = self
                .0
                .query::<Data>(
                    QUERY,
                    Some(json_map! {
                        "block" => block_number,
                        "pageSize" => QUERY_PAGE_SIZE,
                        "lastId" => last_id,
                    }),
                )
                .await?
                .pools;
            let has_next_page = page.len() == QUERY_PAGE_SIZE;

            results.reserve(page.len());
            for pool in page {
                results.push(pool.into_registered(block_number)?);
            }

            has_next_page
        } {}

        Ok((block_number, results))
    }

    /// Retrieves a recent block number for which it is safe to assume no
    /// reorgs will happen.
    async fn get_safe_block(&self) -> Result<u64> {
        use block_number_query::*;

        // Ideally we would want to use block hash here so that we can check
        // that there indeed is no reorg. However, it does not seem possible to
        // retrieve historic block hashes just from the subgraph (it always
        // returns `null`).
        Ok(self
            .0
            .query::<Data>(QUERY, None)
            .await?
            .meta
            .block
            .number
            .saturating_sub(REORG_THRESHOLD))
    }
}

mod pools_query {
    use crate::balancer::{pool_storage::RegisteredWeightedPool, swap::fixed_point::Bfp};
    use anyhow::{anyhow, ensure, Result};
    use ethcontract::{H160, H256};
    use serde::Deserialize;

    pub const QUERY: &str = r#"
        query Pools($block: Int, $pageSize: Int, $lastId: ID) {
            pools(
                block: { number: $block }
                first: $pageSize
                where: {
                    id_gt: $lastId
                    poolType: Weighted
                }
            ) {
                id
                address
                tokens {
                    address
                    decimals
                    weight
                }
                historicalValues(
                    first: 1
                    orderBy: block
                ) {
                    block
                }
            }
        }
    "#;

    #[derive(Debug, Deserialize, PartialEq)]
    pub struct Data {
        pub pools: Vec<Pool>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub struct Pool {
        pub id: H256,
        pub address: H160,
        pub tokens: Vec<Token>,
        pub historical_values: Vec<HistoricalValue>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    pub struct Token {
        pub address: H160,
        pub decimals: u8,
        #[serde(with = "serde_with::rust::display_fromstr")]
        pub weight: Bfp,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    pub struct HistoricalValue {
        #[serde(with = "serde_with::rust::display_fromstr")]
        pub block: u64,
    }

    impl Pool {
        pub fn into_registered(self, block_fetched: u64) -> Result<RegisteredWeightedPool> {
            // The Balancer subgraph does not contain information for the block
            // in which a pool was created. Instead, we assume it is the block
            // that the data was fetched or the oldest historical liquidity
            // values, depending on whether the latter is available.
            let block_created_guesstimate = self
                .historical_values
                .get(0)
                .map(|value| value.block)
                .unwrap_or(block_fetched);
            ensure!(
                block_created_guesstimate <= block_fetched,
                "historical data more recent than fetched block",
            );

            let token_count = self.tokens.len();
            self.tokens.iter().try_fold(
                RegisteredWeightedPool {
                    pool_id: self.id,
                    pool_address: self.address,
                    tokens: Vec::with_capacity(token_count),
                    normalized_weights: Vec::with_capacity(token_count),
                    scaling_exponents: Vec::with_capacity(token_count),
                    block_created: block_created_guesstimate,
                },
                |mut registered_pool, token| {
                    registered_pool.tokens.push(token.address);
                    registered_pool
                        .normalized_weights
                        .push(token.weight.as_uint256());
                    registered_pool.scaling_exponents.push(
                        18u8.checked_sub(token.decimals).ok_or_else(|| {
                            anyhow!("unsupported token with more than 18 decimals")
                        })?,
                    );
                    Ok(registered_pool)
                },
            )
        }
    }
}

mod block_number_query {
    use serde::Deserialize;

    pub const QUERY: &str = r#"{
        _meta {
            block { number }
        }
    }"#;

    #[derive(Debug, Deserialize, PartialEq)]
    pub struct Data {
        #[serde(rename = "_meta")]
        pub meta: Meta,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    pub struct Meta {
        pub block: Block,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    pub struct Block {
        pub number: u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balancer::swap::fixed_point::Bfp;
    use ethcontract::{H160, H256};

    #[test]
    fn decode_pools_data() {
        use pools_query::*;

        assert_eq!(
            serde_json::from_value::<Data>(json!({
                "pools": [
                    {
                        "address": "0x2222222222222222222222222222222222222222",
                        "historicalValues": [{ "block": "42" }],
                        "id": "0x1111111111111111111111111111111111111111111111111111111111111111",
                        "tokens": [
                            {
                                "address": "0x3333333333333333333333333333333333333333",
                                "decimals": 3,
                                "weight": "0.5"
                            },
                            {
                                "address": "0x4444444444444444444444444444444444444444",
                                "decimals": 4,
                                "weight": "0.5"
                            },
                        ],
                    },
                ],
            }))
            .unwrap(),
            Data {
                pools: vec![Pool {
                    id: H256([0x11; 32]),
                    address: H160([0x22; 20]),
                    tokens: vec![
                        Token {
                            address: H160([0x33; 20]),
                            decimals: 3,
                            weight: Bfp::from_wei(500_000_000_000_000_000u128.into()),
                        },
                        Token {
                            address: H160([0x44; 20]),
                            decimals: 4,
                            weight: Bfp::from_wei(500_000_000_000_000_000u128.into()),
                        },
                    ],
                    historical_values: vec![HistoricalValue { block: 42 }],
                }],
            }
        );
    }

    #[test]
    fn decode_block_number_data() {
        use block_number_query::*;

        assert_eq!(
            serde_json::from_value::<Data>(json!({
                "_meta": {
                    "block": {
                        "number": 42,
                    },
                },
            }))
            .unwrap(),
            Data {
                meta: Meta {
                    block: Block { number: 42 }
                }
            }
        );
    }

    #[test]
    fn convert_pool_to_registered_pool() {
        use pools_query::*;

        let pool = Pool {
            id: H256([2; 32]),
            address: H160([1; 20]),
            tokens: vec![
                Token {
                    address: H160([2; 20]),
                    decimals: 1,
                    weight: "1.337".parse().unwrap(),
                },
                Token {
                    address: H160([3; 20]),
                    decimals: 2,
                    weight: "4.2".parse().unwrap(),
                },
            ],
            historical_values: vec![HistoricalValue { block: 1 }],
        };

        assert_eq!(
            pool.into_registered(2).unwrap(),
            RegisteredWeightedPool {
                pool_id: H256([2; 32]),
                pool_address: H160([1; 20]),
                tokens: vec![H160([2; 20]), H160([3; 20])],
                scaling_exponents: vec![17, 16],
                normalized_weights: vec![
                    1_337_000_000_000_000_000u128.into(),
                    4_200_000_000_000_000_000u128.into(),
                ],
                block_created: 1,
            }
        );
    }

    #[test]
    fn pool_conversion_with_missing_historical_data() {
        use pools_query::*;

        let pool = Pool {
            id: H256([2; 32]),
            address: H160([1; 20]),
            tokens: vec![],
            historical_values: vec![],
        };

        assert_eq!(
            pool.into_registered(2).unwrap(),
            RegisteredWeightedPool {
                pool_id: H256([2; 32]),
                pool_address: H160([1; 20]),
                tokens: vec![],
                scaling_exponents: vec![],
                normalized_weights: vec![],
                block_created: 2,
            }
        );
    }

    #[test]
    fn pool_conversion_inconsistent_block_data() {
        use pools_query::*;

        let pool = Pool {
            id: H256([2; 32]),
            address: H160([1; 20]),
            tokens: vec![],
            historical_values: vec![HistoricalValue { block: 3 }],
        };
        assert!(pool.into_registered(2).is_err());
    }

    #[test]
    fn pool_conversion_invalid_decimals() {
        use pools_query::*;

        let pool = Pool {
            id: H256([2; 32]),
            address: H160([1; 20]),
            tokens: vec![Token {
                address: H160([2; 20]),
                decimals: 19,
                weight: "1.337".parse().unwrap(),
            }],
            historical_values: vec![],
        };
        assert!(pool.into_registered(2).is_err());
    }

    #[tokio::test]
    #[ignore]
    async fn balancer_subgraph_query() {
        let client = BalancerSubgraphClient::for_chain(1).unwrap();
        let (block_number, pools) = client.get_weighted_pools().await.unwrap();
        println!("{:#?}", pools);
        println!(
            "Retrieved {} total pools at block {}",
            pools.len(),
            block_number,
        );
    }
}
