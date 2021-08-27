//! Module containing The Graph API client used for retrieving Balancer weighted
//! pools from the Balancer V2 subgraph.
//!
//! The pools retrieved from this client are used to prime the graph event store
//! to reduce start-up time. We do not use this in general for retrieving pools
//! as to:
//! - not rely on external services
//! - ensure that we are using the latest up-to-date pool data by using events
//!   from the node

use crate::sources::balancer::graph_api::pools_query::{PoolData, StableToken, WeightedToken};
use crate::{
    event_handling::MAX_REORG_BLOCK_COUNT, sources::balancer::pool_storage::RegisteredPool,
    subgraph::SubgraphClient,
};
use anyhow::{bail, Result};
use ethcontract::jsonrpc::serde::de::DeserializeOwned;
use ethcontract::{H160, H256};
use reqwest::Client;
use serde_json::json;
use std::collections::HashMap;

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
    pub fn for_chain(chain_id: u64, client: Client) -> Result<Self> {
        let subgraph_name = match chain_id {
            1 => "balancer-v2",
            4 => "balancer-rinkeby-v2",
            _ => bail!("unsupported chain {}", chain_id),
        };
        Ok(Self(SubgraphClient::new(
            "balancer-labs",
            subgraph_name,
            client,
        )?))
    }

    // We do paging by last ID instead of using `skip`. This is the
    // suggested approach to paging best performance:
    // <https://thegraph.com/docs/graphql-api#pagination>
    async fn query_graph_for<T: DeserializeOwned>(
        &self,
        block_number: u64,
        query: &str,
    ) -> Result<Vec<PoolData<T>>> {
        let mut result = Vec::new();
        let mut last_id = H256::default();
        while {
            let page = self
                .0
                .query::<pools_query::Data<T>>(
                    query,
                    Some(json_map! {
                        "block" => block_number,
                        "pageSize" => QUERY_PAGE_SIZE,
                        "lastId" => json!(last_id),
                    }),
                )
                .await?
                .pools;

            let has_next_page = page.len() == QUERY_PAGE_SIZE;
            if let Some(last_pool) = page.last() {
                last_id = last_pool.id;
            }
            result.extend(page);
            has_next_page
        } {}
        Ok(result)
    }

    /// Retrieves the list of registered pools from the subgraph.
    pub async fn get_registered_pools(&self) -> Result<RegisteredPools> {
        let block_number = self.get_safe_block().await?;

        let weighted_pools = self
            .query_graph_for::<WeightedToken>(block_number, pools_query::WEIGHTED_POOL_QUERY)
            .await?
            .into_iter()
            .map(|pool_data| {
                (
                    pool_data.factory,
                    pool_data.into_registered_pool(block_number),
                )
            });
        let stable_pools = self
            .query_graph_for::<StableToken>(block_number, pools_query::STABLE_POOL_QUERY)
            .await?
            .into_iter()
            .map(|pool_data| {
                (
                    pool_data.factory,
                    pool_data.into_registered_pool(block_number),
                )
            });

        let mut pools_by_factory = HashMap::<H160, Vec<RegisteredPool>>::new();
        for (factory, pool) in weighted_pools.chain(stable_pools) {
            let pool = match pool {
                Ok(pool) => pool,
                // Technically this should never happen and should only ever be from
                // a token with more than 18 decimals (not supported by balancer contracts)
                // https://github.com/balancer-labs/balancer-v2-monorepo/blob/deployments-latest/pkg/pool-utils/contracts/BasePool.sol#L476-L487
                Err(err) => bail!("failed conversion to registered pool with {}", err),
            };
            pools_by_factory
                .entry(factory.unwrap_or_default())
                .or_default()
                .push(pool);
        }

        Ok(RegisteredPools {
            fetched_block_number: block_number,
            pools_by_factory,
        })
    }

    /// Retrieves a recent block number for which it is safe to assume no
    /// reorgs will happen.
    async fn get_safe_block(&self) -> Result<u64> {
        // Ideally we would want to use block hash here so that we can check
        // that there indeed is no reorg. However, it does not seem possible to
        // retrieve historic block hashes just from the subgraph (it always
        // returns `null`).
        Ok(self
            .0
            .query::<block_number_query::Data>(block_number_query::QUERY, None)
            .await?
            .meta
            .block
            .number
            .saturating_sub(MAX_REORG_BLOCK_COUNT))
    }
}

/// Result of the registered stable pool query.
pub struct RegisteredPools {
    /// The block number that the data was fetched, and for which the registered
    /// weighted pools can be considered up to date.
    pub fetched_block_number: u64,
    /// The registered Pools
    pub pools_by_factory: HashMap<H160, Vec<RegisteredPool>>,
}

mod pools_query {
    use crate::sources::balancer::{
        pool_storage::{RegisteredPool, RegisteredStablePool, RegisteredWeightedPool},
        swap::fixed_point::Bfp,
    };
    use anyhow::{anyhow, Result};
    use ethcontract::{H160, H256};
    use serde::Deserialize;

    pub const WEIGHTED_POOL_QUERY: &str = r#"
        query Pools($block: Int, $pageSize: Int, $lastId: ID) {
            pools(
                block: { number: $block }
                first: $pageSize
                where: {
                    id_gt: $lastId
                    poolType: "Weighted"
                }
            ) {
                id
                address
                factory
                tokens {
                    address
                    decimals
                    weight
                }
            }
        }
    "#;

    pub const STABLE_POOL_QUERY: &str = r#"
        query Pools($block: Int, $pageSize: Int, $lastId: ID) {
            pools(
                block: { number: $block }
                first: $pageSize
                where: {
                    id_gt: $lastId
                    poolType: "Stable"
                }
            ) {
                id
                address
                factory
                tokens {
                    address
                    decimals
                }
            }
        }
    "#;

    #[derive(Debug, Deserialize, PartialEq)]
    pub struct Data<T> {
        pub pools: Vec<PoolData<T>>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    pub struct PoolData<T> {
        pub id: H256,
        pub address: H160,
        pub factory: Option<H160>,
        pub tokens: Vec<T>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    pub struct WeightedToken {
        pub address: H160,
        pub decimals: u8,
        #[serde(with = "serde_with::rust::display_fromstr")]
        pub weight: Bfp,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    pub struct StableToken {
        pub address: H160,
        pub decimals: u8,
    }

    impl PoolData<WeightedToken> {
        pub fn into_registered_pool(self, block_fetched: u64) -> Result<RegisteredPool> {
            // The Balancer subgraph does not contain information for the block
            // in which a pool was created. Instead, we just use the block that
            // the data was fetched for, as the created block is guaranteed to
            // be older than that.
            let block_created_upper_bound = block_fetched;

            let token_count = self.tokens.len();
            self.tokens
                .iter()
                .try_fold(
                    RegisteredWeightedPool {
                        pool_id: self.id,
                        pool_address: self.address,
                        tokens: Vec::with_capacity(token_count),
                        normalized_weights: Vec::with_capacity(token_count),
                        scaling_exponents: Vec::with_capacity(token_count),
                        block_created: block_created_upper_bound,
                    },
                    |mut pool, token| {
                        pool.tokens.push(token.address);
                        pool.normalized_weights.push(token.weight);
                        pool.scaling_exponents
                            .push(18u8.checked_sub(token.decimals).ok_or_else(|| {
                                anyhow!("unsupported token with more than 18 decimals")
                            })?);
                        Ok(pool)
                    },
                )
                .map(RegisteredPool::Weighted)
        }
    }

    impl PoolData<StableToken> {
        pub fn into_registered_pool(self, block_fetched: u64) -> Result<RegisteredPool> {
            // The Balancer subgraph does not contain information for the block
            // in which a pool was created. Instead, we just use the block that
            // the data was fetched for, as the created block is guaranteed to
            // be older than that.
            let block_created_upper_bound = block_fetched;

            let token_count = self.tokens.len();
            self.tokens
                .iter()
                .try_fold(
                    RegisteredStablePool {
                        pool_id: self.id,
                        pool_address: self.address,
                        tokens: Vec::with_capacity(token_count),
                        scaling_exponents: Vec::with_capacity(token_count),
                        block_created: block_created_upper_bound,
                    },
                    |mut pool, token| {
                        pool.tokens.push(token.address);
                        pool.scaling_exponents
                            .push(18u8.checked_sub(token.decimals).ok_or_else(|| {
                                anyhow!("unsupported token with more than 18 decimals")
                            })?);
                        Ok(pool)
                    },
                )
                .map(RegisteredPool::Stable)
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
    use crate::sources::balancer::pool_storage::{RegisteredStablePool, RegisteredWeightedPool};
    use crate::sources::balancer::swap::fixed_point::Bfp;
    use ethcontract::{H160, H256};

    #[test]
    fn decode_pools_data() {
        use pools_query::*;

        assert_eq!(
            serde_json::from_value::<Data<WeightedToken>>(json!({
                "pools": [
                    {
                        "address": "0x2222222222222222222222222222222222222222",
                        "id": "0x1111111111111111111111111111111111111111111111111111111111111111",
                        "factory": "0x5555555555555555555555555555555555555555",
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
                pools: vec![PoolData {
                    id: H256([0x11; 32]),
                    address: H160([0x22; 20]),
                    factory: Some(H160([0x55; 20])),
                    tokens: vec![
                        WeightedToken {
                            address: H160([0x33; 20]),
                            decimals: 3,
                            weight: Bfp::from_wei(500_000_000_000_000_000u128.into()),
                        },
                        WeightedToken {
                            address: H160([0x44; 20]),
                            decimals: 4,
                            weight: Bfp::from_wei(500_000_000_000_000_000u128.into()),
                        },
                    ],
                }],
            }
        );

        assert_eq!(
            serde_json::from_value::<Data<StableToken>>(json!({
                "pools": [
                    {
                        "address": "0x2222222222222222222222222222222222222222",
                        "id": "0x1111111111111111111111111111111111111111111111111111111111111111",
                        "factory": "0x5555555555555555555555555555555555555555",
                        "tokens": [
                            {
                                "address": "0x3333333333333333333333333333333333333333",
                                "decimals": 3,
                            },
                            {
                                "address": "0x4444444444444444444444444444444444444444",
                                "decimals": 4,
                            },
                        ],
                    },
                ],
            }))
            .unwrap(),
            Data {
                pools: vec![PoolData {
                    id: H256([0x11; 32]),
                    address: H160([0x22; 20]),
                    factory: Some(H160([0x55; 20])),
                    tokens: vec![
                        StableToken {
                            address: H160([0x33; 20]),
                            decimals: 3,
                        },
                        StableToken {
                            address: H160([0x44; 20]),
                            decimals: 4,
                        },
                    ],
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
        // Note that this test also demonstrates unreachable code is indeed unreachable
        use pools_query::*;

        let weighted_pool_data = PoolData {
            id: H256([2; 32]),
            address: H160([1; 20]),
            factory: None,
            tokens: vec![
                WeightedToken {
                    address: H160([2; 20]),
                    decimals: 1,
                    weight: "1.337".parse().unwrap(),
                },
                WeightedToken {
                    address: H160([3; 20]),
                    decimals: 2,
                    weight: "4.2".parse().unwrap(),
                },
            ],
        };

        assert_eq!(
            weighted_pool_data.into_registered_pool(42).unwrap(),
            RegisteredPool::Weighted(RegisteredWeightedPool {
                pool_id: H256([2; 32]),
                pool_address: H160([1; 20]),
                tokens: vec![H160([2; 20]), H160([3; 20])],
                scaling_exponents: vec![17, 16],
                normalized_weights: vec![
                    Bfp::from_wei(1_337_000_000_000_000_000u128.into()),
                    Bfp::from_wei(4_200_000_000_000_000_000u128.into()),
                ],
                block_created: 42,
            })
        );

        let stable_pool_data = PoolData {
            id: H256([2; 32]),
            address: H160([1; 20]),
            factory: None,
            tokens: vec![
                StableToken {
                    address: H160([2; 20]),
                    decimals: 1,
                },
                StableToken {
                    address: H160([3; 20]),
                    decimals: 2,
                },
            ],
        };

        assert_eq!(
            stable_pool_data.into_registered_pool(42).unwrap(),
            RegisteredPool::Stable(RegisteredStablePool {
                pool_id: H256([2; 32]),
                pool_address: H160([1; 20]),
                tokens: vec![H160([2; 20]), H160([3; 20])],
                scaling_exponents: vec![17, 16],
                block_created: 42,
            })
        );
    }

    #[test]
    fn pool_conversion_invalid_decimals() {
        use pools_query::*;

        let pool = PoolData {
            id: H256([2; 32]),
            address: H160([1; 20]),
            factory: None,
            tokens: vec![WeightedToken {
                address: H160([2; 20]),
                decimals: 19,
                weight: "1.337".parse().unwrap(),
            }],
        };
        assert!(pool.into_registered_pool(2).is_err());
    }

    #[tokio::test]
    #[ignore]
    async fn balancer_subgraph_query() {
        let client = BalancerSubgraphClient::for_chain(1, Client::new()).unwrap();
        let pools = client.get_registered_pools().await.unwrap();
        println!(
            "Retrieved {} total pools at block {}",
            pools
                .pools_by_factory
                .iter()
                .map(|(factory, pool)| {
                    println!("Retrieved {} pools for factory at {}", pool.len(), factory);
                    pool.len()
                })
                .sum::<usize>(),
            pools.fetched_block_number,
        );
    }
}
