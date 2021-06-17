use crate::{
    current_block::CurrentBlockStream,
    maintenance::Maintaining,
    balancer::{pool_fetching::PoolReserveFetching, pool_storage::WeightedPool},
    recent_block_cache::{
        Block, CacheConfig, CacheFetching, CacheKey, CacheMetrics, RecentBlockCache,
    },
};
use anyhow::Result;
use std::{collections::HashSet, sync::Arc};
use ethcontract::H256;

pub trait PoolCacheMetrics: Send + Sync {
    fn pools_fetched(&self, cache_hits: usize, cache_misses: usize);
}

pub struct BalancerPoolCache(
    RecentBlockCache<H256, WeightedPool, Box<dyn PoolReserveFetching>, Arc<dyn PoolCacheMetrics>>,
);

impl CacheKey<WeightedPool> for H256 {
    fn first_ord() -> Self {
        H256::zero()
    }

    fn for_value(value: &WeightedPool) -> Self {
        value.pool_id
    }
}

#[async_trait::async_trait]
impl CacheFetching<H256, WeightedPool> for Box<dyn PoolReserveFetching> {
    async fn fetch_values(&self, keys: HashSet<H256>, block: Block) -> Result<Vec<WeightedPool>> {
        self.fetch(keys, block).await
    }
}

impl CacheMetrics for Arc<dyn PoolCacheMetrics> {
    fn entries_fetched(&self, cache_hits: usize, cache_misses: usize) {
        self.pools_fetched(cache_hits, cache_misses)
    }
}

impl BalancerPoolCache {
    /// Creates a new pool cache.
    pub fn new(
        config: CacheConfig,
        fetcher: Box<dyn PoolReserveFetching>,
        block_stream: CurrentBlockStream,
        metrics: Arc<dyn PoolCacheMetrics>,
    ) -> Result<Self> {
        Ok(Self(RecentBlockCache::new(
            config,
            fetcher,
            block_stream,
            metrics,
        )?))
    }
}

#[async_trait::async_trait]
impl PoolReserveFetching for BalancerPoolCache {
    async fn fetch(&self, pool_ids: HashSet<H256>, block: Block) -> Result<Vec<WeightedPool>> {
        self.0.fetch(pool_ids, block).await
    }
}

#[async_trait::async_trait]
impl Maintaining for BalancerPoolCache {
    async fn run_maintenance(&self) -> Result<()> {
        self.0.update_cache().await
    }
}
