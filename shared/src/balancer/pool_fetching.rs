use anyhow::Result;
use model::TokenPair;
use std::collections::HashSet;

use crate::balancer::event_handler::{PoolRegistry, PoolSpecialization, RegisteredPool};
use crate::pool_fetching::{handle_contract_error, Block, MAX_BATCH_SIZE};
use crate::Web3;
use contracts::BalancerV2Vault;
use ethcontract::batch::CallBatch;
use ethcontract::errors::MethodError;
use ethcontract::{BlockId, Bytes, H160, H256, U256};
use itertools::Itertools;

pub struct WeightedPool {
    pub pool_id: H256,
    pub pool_address: H160,
    pub normalized_weights: Vec<U256>,
    pub specialization: PoolSpecialization,
    pub tokens: Vec<H160>,
    pub reserves: Vec<U256>,
}

#[async_trait::async_trait]
pub trait WeightedPoolFetching: Send + Sync {
    async fn fetch(
        &self,
        token_pairs: HashSet<TokenPair>,
        at_block: Block,
    ) -> Result<Vec<WeightedPool>>;
}

pub struct BalancerPoolFetcher {
    pool_data: PoolRegistry,
    vault: BalancerV2Vault,
    web3: Web3,
}

#[async_trait::async_trait]
impl WeightedPoolFetching for BalancerPoolFetcher {
    async fn fetch(
        &self,
        token_pairs: HashSet<TokenPair>,
        at_block: Block,
    ) -> Result<Vec<WeightedPool>> {
        let mut batch = CallBatch::new(self.web3.transport());
        let block = BlockId::Number(at_block.into());
        let futures = token_pairs
            .into_iter()
            .flat_map(|pair| self.pool_data.pools_containing_pair(pair))
            .unique_by(|pool| pool.pool_id)
            .map(|weighted_pool| {
                let reserves = self
                    .vault
                    .get_pool_tokens(Bytes(weighted_pool.pool_id.0))
                    .block(block)
                    .batch_call(&mut batch);
                async move {
                    FetchedWeightedPool {
                        pool_data: weighted_pool,
                        reserves: reserves.await,
                    }
                }
            })
            .collect::<Vec<_>>();
        batch.execute_all(MAX_BATCH_SIZE).await;

        let mut results = Vec::new();
        for future in futures {
            results.push(future.await);
        }
        handle_results(results)
    }
}

/// An internal temporary struct used during pool fetching to handle errors.
struct FetchedWeightedPool {
    pool_data: RegisteredPool,
    /// getPoolTokens returns (Tokens, Balances, LastBlockUpdated)
    reserves: Result<(Vec<H160>, Vec<U256>, U256), MethodError>,
}

fn handle_results(results: Vec<FetchedWeightedPool>) -> Result<Vec<WeightedPool>> {
    results
        .into_iter()
        .try_fold(Vec::new(), |mut acc, fetched_pool| {
            let reserves = match handle_contract_error(fetched_pool.reserves)? {
                // We only keep the balances entry of reserves query.
                Some(reserves) => reserves.1,
                None => return Ok(acc),
            };
            acc.push(WeightedPool {
                pool_id: fetched_pool.pool_data.pool_id,
                pool_address: fetched_pool.pool_data.pool_address,
                normalized_weights: fetched_pool.pool_data.normalized_weights,
                specialization: fetched_pool.pool_data.specialization,
                tokens: fetched_pool.pool_data.tokens,
                reserves,
            });
            Ok(acc)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ethcontract_error;

    #[test]
    fn pool_fetcher_forwards_node_error() {
        let results = vec![FetchedWeightedPool {
            pool_data: RegisteredPool::test_instance(),
            reserves: Err(ethcontract_error::testing_node_error()),
        }];
        assert!(handle_results(results).is_err());
    }

    #[test]
    fn pool_fetcher_skips_contract_error() {
        let results = vec![
            FetchedWeightedPool {
                pool_data: RegisteredPool::test_instance(),
                reserves: Err(ethcontract_error::testing_contract_error()),
            },
            FetchedWeightedPool {
                pool_data: RegisteredPool::test_instance(),
                reserves: Ok((vec![], vec![], U256::zero())),
            },
        ];
        assert_eq!(handle_results(results).unwrap().len(), 1);
    }
}
