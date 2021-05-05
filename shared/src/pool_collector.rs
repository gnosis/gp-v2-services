use crate::pool_fetching::{Pool, PoolFetcher, PoolFetching};
use model::TokenPair;
use std::collections::HashSet;

pub struct PoolAggregator {
    pub pool_fetchers: Vec<PoolFetcher>,
}

#[async_trait::async_trait]
impl PoolFetching for PoolAggregator {
    async fn fetch(&self, token_pairs: HashSet<TokenPair>) -> Vec<Pool> {
        let mut pools = vec![];
        for fetcher in self.pool_fetchers.iter() {
            pools.extend(fetcher.fetch(token_pairs.clone()).await);
        }
        pools
    }
}
