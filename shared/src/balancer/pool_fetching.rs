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

pub struct FetchedWeightedPool {
    pub pool: WeightedPool,
    /// getPoolTokens returns (Tokens, Balances, LastBlockUpdated)
    pub reserves: Result<(Vec<H160>, Vec<U256>, U256), MethodError>,
}

#[async_trait::async_trait]
pub trait WeightedPoolFetching: Send + Sync {
    async fn fetch(
        &self,
        token_pairs: HashSet<TokenPair>,
        at_block: Block,
    ) -> Result<Vec<FetchedWeightedPool>>;
}

fn handle_results(results: Vec<FetchedWeightedPool>) -> Result<Vec<FetchedWeightedPool>> {
    results.into_iter().try_fold(Vec::new(), |acc, pool| {
        match handle_contract_error(pool.reserves)? {
            Some(reserves) => reserves,
            None => return Ok(acc),
        };
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
                    FetchedWeightedPool {
                        pool: weighted_pool,
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
