use anyhow::Result;
use model::TokenPair;
use std::collections::HashSet;

use crate::balancer::event_handler::{BalancerPoolStore, WeightedPool};
use crate::pool_fetching::{handle_contract_error, Block, MAX_BATCH_SIZE};
use crate::Web3;
use contracts::BalancerV2Vault;
use ethcontract::batch::CallBatch;
use ethcontract::errors::MethodError;
use ethcontract::{BlockId, Bytes, H160, U256};
use itertools::Itertools;

pub struct PreFetchedWeightedPool {
    pool_data: WeightedPool,
    /// getPoolTokens returns (Tokens, Balances, LastBlockUpdated)
    reserves: Result<(Vec<H160>, Vec<U256>, U256), MethodError>,
}

pub struct FetchedWeightedPool {
    pub pool_data: WeightedPool,
    pub reserves: Vec<U256>,
}

#[async_trait::async_trait]
pub trait WeightedPoolFetching: Send + Sync {
    async fn fetch(
        &self,
        token_pairs: HashSet<TokenPair>,
        at_block: Block,
    ) -> Result<Vec<FetchedWeightedPool>>;
}

fn handle_results(results: Vec<PreFetchedWeightedPool>) -> Result<Vec<FetchedWeightedPool>> {
    results
        .into_iter()
        .try_fold(Vec::new(), |mut acc, pre_fetched_pool| {
            let reserves = match handle_contract_error(pre_fetched_pool.reserves)? {
                // We only keep the balances entry of reserves query.
                Some(reserves) => reserves.1,
                None => return Ok(acc),
            };
            acc.push(FetchedWeightedPool {
                pool_data: pre_fetched_pool.pool_data,
                reserves,
            });
            Ok(acc)
        })
}

pub struct BalancerPoolFetcher {
    pool_data: BalancerPoolStore,
    vault: BalancerV2Vault,
    web3: Web3,
}

#[async_trait::async_trait]
impl WeightedPoolFetching for BalancerPoolFetcher {
    async fn fetch(
        &self,
        token_pairs: HashSet<TokenPair>,
        at_block: Block,
    ) -> Result<Vec<FetchedWeightedPool>> {
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
                    PreFetchedWeightedPool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ethcontract_error;

    #[test]
    fn pool_fetcher_forwards_node_error() {
        let results = vec![PreFetchedWeightedPool {
            pool_data: WeightedPool::test_instance(),
            reserves: Err(ethcontract_error::testing_node_error()),
        }];
        assert!(handle_results(results).is_err());
    }

    #[test]
    fn pool_fetcher_skips_contract_error() {
        let results = vec![
            PreFetchedWeightedPool {
                pool_data: WeightedPool::test_instance(),
                reserves: Err(ethcontract_error::testing_contract_error()),
            },
            PreFetchedWeightedPool {
                pool_data: WeightedPool::test_instance(),
                reserves: Ok((vec![], vec![], U256::zero())),
            },
        ];
        assert_eq!(handle_results(results).unwrap().len(), 1);
    }
}
