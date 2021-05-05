use crate::amm_pair_provider::AmmPairProvider;
use crate::pool_fetching::{Pool, PoolFetcher, PoolFetching};
use crate::Web3;
use model::TokenPair;
use std::collections::HashSet;
use std::sync::Arc;

pub struct PoolCollector {
    pub pool_fetchers: Vec<PoolFetcher>,
}

impl PoolCollector {
    pub fn new(pair_providers: Vec<Arc<dyn AmmPairProvider>>, web3: Web3) -> Self {
        let mut pool_fetchers = vec![];
        for pair_provider in pair_providers {
            pool_fetchers.push(PoolFetcher {
                pair_provider,
                web3: web3.clone(),
            })
        }
        Self { pool_fetchers }
    }
}

#[async_trait::async_trait]
impl PoolFetching for PoolCollector {
    async fn fetch(&self, token_pairs: HashSet<TokenPair>) -> Vec<Pool> {
        let mut pools = vec![];
        for fetcher in self.pool_fetchers.iter() {
            pools.extend(fetcher.fetch(token_pairs.clone()).await);
        }
        pools
    }
}
