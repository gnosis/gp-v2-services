use anyhow::{Context, Result};
use lru::LruCache;
use model::TokenPair;
use shared::{
    maintenance::Maintaining,
    pool_fetching::{LatestPoolFetching, Pool},
};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

const CACHED_TOKEN_PAIRS: usize = 100;

/// Pool fetcher optimized for use in the order book where we only care about the most recent state
// of the pools.
pub struct OrderbookPoolFetcher {
    mutexed: Mutex<Mutexed>,
    inner: Box<dyn LatestPoolFetching>,
}

// Design:
// The design of this module is driven by the need to always return pools quickly so that end users
// going through the api do not have to wait longer than necessary:
// - The mutex is never held while waiting on an async operation (getting pools from the node).
// - Updating the cache is decoupled from normal pool fetches.
// When pools are requested we mark all those pools as recently used which potentially evicts other
// pairs from the pair lru cache. Cache misses are fetched and inserted into the cache.
// Then when update runs the next time we request all recently used pairs, insert them into the
// the cache and shrink the cache to only contain recently used pairs.

impl OrderbookPoolFetcher {
    pub fn new(inner: Box<dyn LatestPoolFetching>) -> Self {
        Self::with_cache_size(inner, CACHED_TOKEN_PAIRS)
    }

    fn with_cache_size(inner: Box<dyn LatestPoolFetching>, cache_size: usize) -> Self {
        Self {
            mutexed: Mutex::new(Mutexed {
                pairs: LruCache::new(cache_size),
                pools: HashMap::new(),
            }),
            inner,
        }
    }

    pub async fn update_cache(&self) -> Result<()> {
        let pairs = self.mutexed.lock().unwrap().recently_used_pairs();
        // Mutex is not held while we fetch pools.
        let pools = self.inner.fetch_latest(pairs.clone()).await?;
        let pools = group_pools_by_token_pair(&pairs, &pools);
        // It is possible that a simultaneous fetch has changed the pair lru cache and added new
        // pools to the pool cache. To not undo this it is important that we do not delete anything
        // from the pool cache. We only extend it.
        {
            let mut mutexed = self.mutexed.lock().unwrap();
            mutexed.cache(pools);
            mutexed.shrink_cache();
        }
        Ok(())
    }

    fn mark_used_and_get_hits_and_misses(
        &self,
        pairs: &HashSet<TokenPair>,
    ) -> (Vec<Pool>, HashSet<TokenPair>) {
        let mut cache_hits = Vec::new();
        let mut cache_misses = HashSet::new();
        let mut mutexed = self.mutexed.lock().unwrap();
        for pair in pairs {
            mutexed.mark_used(*pair);
            if let Some(pools) = mutexed.pools.get(pair) {
                cache_hits.extend_from_slice(&pools)
            } else {
                cache_misses.insert(*pair);
            }
        }
        (cache_hits, cache_misses)
    }
}

#[async_trait::async_trait]
impl LatestPoolFetching for OrderbookPoolFetcher {
    async fn fetch_latest(&self, token_pairs: HashSet<TokenPair>) -> Result<Vec<Pool>> {
        let (mut cache_hits, cache_misses) = self.mark_used_and_get_hits_and_misses(&token_pairs);
        if cache_misses.is_empty() {
            return Ok(cache_hits);
        }
        // Mutex is not held while fetching pools.
        let pools = self.inner.fetch_latest(cache_misses.clone()).await?;
        self.mutexed
            .lock()
            .unwrap()
            .cache(group_pools_by_token_pair(&cache_misses, &pools));
        // Shrinking the cache if expensive so we do not do it here. It will happen on next update.
        cache_hits.extend_from_slice(&pools);
        Ok(cache_hits)
    }
}

#[async_trait::async_trait]
impl Maintaining for OrderbookPoolFetcher {
    async fn run_maintenance(&self) -> Result<()> {
        self.update_cache()
            .await
            .context("failed to update pool cache")
    }
}

#[derive(Debug)]
struct Mutexed {
    // The last N recently used pairs.
    pairs: LruCache<TokenPair, ()>,
    // The cache of those pairs.
    pools: HashMap<TokenPair, Vec<Pool>>,
}

impl Mutexed {
    fn recently_used_pairs(&self) -> HashSet<TokenPair> {
        self.pairs.iter().map(|(pair, _)| *pair).collect()
    }

    /// Drop pools for pairs that haven't been recently used.
    fn shrink_cache(&mut self) {
        // We use self in the closure so to appease the borrow checker we temporarily remove pools
        // from self.
        let mut pools = std::mem::take(&mut self.pools);
        pools.retain(|pair, _| self.pairs.contains(pair));
        self.pools = pools;
    }

    /// Insert new or overwrite existing pools.
    fn cache(&mut self, new_pools: HashMap<TokenPair, Vec<Pool>>) {
        self.pools.extend(new_pools);
    }

    /// Update the recently used time.
    fn mark_used(&mut self, pair: TokenPair) {
        self.pairs.put(pair, ());
    }
}

fn group_pools_by_token_pair(
    pairs: &HashSet<TokenPair>,
    pools: &[Pool],
) -> HashMap<TokenPair, Vec<Pool>> {
    // It is important to cache empty vectors too so that we remember that there were no pools for
    // that pair and not try to fetch them again next time.
    let mut map = pairs
        .iter()
        .map(|&pair| (pair, Vec::new()))
        .collect::<HashMap<_, _>>();
    for pool in pools {
        // Unwrap because PoolFetching should never return pools for pairs that weren't requested.
        map.get_mut(&pool.tokens).unwrap().push(*pool);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;
    use primitive_types::H160;
    use std::sync::Arc;

    #[derive(Default)]
    struct FakePoolFetcher(Arc<Mutex<Vec<Pool>>>);
    #[async_trait::async_trait]
    impl LatestPoolFetching for FakePoolFetcher {
        async fn fetch_latest(&self, _: HashSet<TokenPair>) -> Result<Vec<Pool>> {
            Ok(self.0.lock().unwrap().clone())
        }
    }

    #[test]
    fn cache_works() {
        let pairs = [
            TokenPair::new(H160::from_low_u64_le(0), H160::from_low_u64_le(1)).unwrap(),
            TokenPair::new(H160::from_low_u64_le(1), H160::from_low_u64_le(2)).unwrap(),
            TokenPair::new(H160::from_low_u64_le(2), H160::from_low_u64_le(3)).unwrap(),
        ];
        let inner = FakePoolFetcher::default();
        let pools = inner.0.clone();
        let cache = OrderbookPoolFetcher::with_cache_size(Box::new(inner), 2);

        // Cache miss lands in cache.
        *pools.lock().unwrap() = vec![
            Pool::uniswap(pairs[0], (0, 0)),
            Pool::uniswap(pairs[1], (0, 0)),
        ];
        let result = cache
            .fetch_latest(pairs[0..2].iter().copied().collect())
            .now_or_never()
            .unwrap()
            .unwrap();
        assert_eq!(result.len(), 2);

        // Now that there is a cache the inner fetcher should be skipped.
        pools.lock().unwrap().clear();
        let result = cache
            .fetch_latest(pairs[0..2].iter().copied().collect())
            .now_or_never()
            .unwrap()
            .unwrap();
        // Still returns the two pools from before even though inner if used would be empty.
        assert_eq!(result.len(), 2);

        // Updates propagate to the cache. We observe the reserves changing.
        *pools.lock().unwrap() = vec![
            Pool::uniswap(pairs[0], (1, 1)),
            Pool::uniswap(pairs[1], (1, 1)),
        ];
        cache.update_cache().now_or_never().unwrap().unwrap();
        pools.lock().unwrap().clear();
        let result = cache
            .fetch_latest(pairs[0..2].iter().copied().collect())
            .now_or_never()
            .unwrap()
            .unwrap();
        // Still returns the two pools from before even though inner if used would be empty.
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].reserves, (1, 1));

        // Evict the first token pair from the pool by requesting the second and third pair.
        pools.lock().unwrap().clear();
        let result = cache
            .fetch_latest(pairs[1..3].iter().copied().collect())
            .now_or_never()
            .unwrap()
            .unwrap();
        assert_eq!(result.len(), 1);
        // Still (1,1) because this pair should be cached.
        assert_eq!(result[0].reserves, (1, 1));
        cache.update_cache().now_or_never().unwrap().unwrap();
        let mutexed = cache.mutexed.lock().unwrap();
        assert_eq!(mutexed.pairs.len(), 2);
        assert!(mutexed.pairs.contains(&pairs[1]));
        assert!(mutexed.pairs.contains(&pairs[2]));
        assert_eq!(mutexed.pools.len(), 2);
        // Both pool caches have correctly cached the empty vector.
        assert!(mutexed.pools.get(&pairs[1]).unwrap().is_empty());
        assert!(mutexed.pools.get(&pairs[2]).unwrap().is_empty());
    }
}
