//! Pool Fetching is primarily concerned with retrieving relevant pools from the `BalancerPoolRegistry`
//! when given a collection of `TokenPair`. Each of these pools are then queried for
//! their `token_balances` and the `PoolFetcher` returns all up-to-date `WeightedPools`
//! to be consumed by external users (e.g. Price Estimators and Solvers).
use crate::{
    current_block::CurrentBlockStream,
    maintenance::Maintaining,
    recent_block_cache::{Block, CacheConfig, RecentBlockCache},
    sources::balancer::{
        event_handler::BalancerPoolRegistry,
        info_fetching::PoolInfoFetcher,
        pool_cache::{BalancerPoolCacheMetrics, BalancerPoolReserveCache, PoolReserveFetcher},
        pool_init::DefaultPoolInitializer,
        pool_storage::{PoolType, RegisteredStablePool, RegisteredWeightedPool},
        swap::fixed_point::Bfp,
    },
    token_info::TokenInfoFetching,
    Web3,
};
use anyhow::{anyhow, Result};
use ethcontract::{H160, H256, U256};
use model::TokenPair;
use reqwest::Client;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenState {
    pub balance: U256,
    pub scaling_exponent: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WeightedPoolState {
    pub token_state: TokenState,
    pub weight: Bfp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BalancerPoolState {
    Weighted(WeightedPoolState),
    Stable(TokenState),
}

impl WeightedPoolState {
    pub fn balance(&self) -> U256 {
        self.token_state.balance
    }

    pub fn scaling_exponent(&self) -> u8 {
        self.token_state.scaling_exponent
    }
}

impl BalancerPoolState {
    pub fn balance(&self) -> U256 {
        match self {
            BalancerPoolState::Weighted(pool) => pool.balance(),
            BalancerPoolState::Stable(pool) => pool.balance,
        }
    }

    pub fn scaling_exponent(&self) -> u8 {
        match self {
            BalancerPoolState::Weighted(pool) => pool.scaling_exponent(),
            BalancerPoolState::Stable(pool) => pool.scaling_exponent,
        }
    }
}

#[derive(Clone, Debug)]
pub enum BalancerPool {
    Weighted(WeightedPool),
    Stable(StablePool),
}

impl BalancerPool {
    pub fn pool_id(&self) -> H256 {
        match self {
            BalancerPool::Weighted(pool) => pool.pool_id,
            BalancerPool::Stable(pool) => pool.pool_id,
        }
    }

    pub fn paused(&self) -> bool {
        match self {
            BalancerPool::Weighted(pool) => pool.paused,
            BalancerPool::Stable(pool) => pool.paused,
        }
    }

    pub fn reserve_keys(&self) -> HashSet<H160> {
        match self {
            BalancerPool::Weighted(pool) => pool.reserves.keys().copied().collect(),
            BalancerPool::Stable(pool) => pool.reserves.keys().copied().collect(),
        }
    }

    pub fn swap_fee_percentage(&self) -> Bfp {
        match self {
            BalancerPool::Weighted(pool) => pool.swap_fee_percentage,
            BalancerPool::Stable(pool) => pool.swap_fee_percentage,
        }
    }

    pub fn pool_type(&self) -> PoolType {
        match self {
            BalancerPool::Weighted(_) => PoolType::Weighted,
            BalancerPool::Stable(_) => PoolType::Stable,
        }
    }

    pub fn reserves(&self) -> HashMap<H160, BalancerPoolState> {
        match self {
            BalancerPool::Weighted(pool) => pool
                .clone()
                .reserves
                .into_iter()
                .map(|(k, v)| (k, BalancerPoolState::Weighted(v)))
                .collect(),
            BalancerPool::Stable(pool) => pool
                .clone()
                .reserves
                .into_iter()
                .map(|(k, v)| (k, BalancerPoolState::Stable(v)))
                .collect(),
        }
    }

    pub fn try_into_weighted(&self) -> Result<WeightedPool> {
        if let BalancerPool::Weighted(pool) = self {
            Ok(pool.clone())
        } else {
            Err(anyhow!("Not a weighted pool!"))
        }
    }

    pub fn try_into_stable(&self) -> Result<StablePool> {
        if let BalancerPool::Stable(pool) = self {
            Ok(pool.clone())
        } else {
            Err(anyhow!("Not a weighted pool!"))
        }
    }

    pub fn is_weighted(&self) -> bool {
        match self {
            BalancerPool::Weighted(_) => true,
            BalancerPool::Stable(_) => false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct WeightedPool {
    pub pool_id: H256,
    pub pool_address: H160,
    pub swap_fee_percentage: Bfp,
    pub reserves: HashMap<H160, WeightedPoolState>,
    pub paused: bool,
}

impl WeightedPool {
    pub fn new(
        pool_data: RegisteredWeightedPool,
        balances: Vec<U256>,
        swap_fee_percentage: Bfp,
        paused: bool,
    ) -> Self {
        let mut reserves = HashMap::new();
        // We expect the weight and token indices are aligned with balances returned from EVM query.
        // If necessary we would also pass the tokens along with the query result,
        // use them and fetch the weights from the registry by token address.
        for (i, balance) in balances.into_iter().enumerate() {
            reserves.insert(
                pool_data.tokens[i],
                WeightedPoolState {
                    token_state: TokenState {
                        balance,
                        scaling_exponent: pool_data.scaling_exponents[i],
                    },
                    weight: pool_data.normalized_weights[i],
                },
            );
        }
        WeightedPool {
            pool_id: pool_data.pool_id,
            pool_address: pool_data.pool_address,
            swap_fee_percentage,
            reserves,
            paused,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StablePool {
    pub pool_id: H256,
    pub pool_address: H160,
    pub swap_fee_percentage: Bfp,
    pub amplification_parameter: U256,
    pub reserves: HashMap<H160, TokenState>,
    pub paused: bool,
}

impl StablePool {
    pub fn new(
        pool_data: RegisteredStablePool,
        balances: Vec<U256>,
        swap_fee_percentage: Bfp,
        amplification_parameter: U256,
        paused: bool,
    ) -> Self {
        let mut reserves = HashMap::new();
        // We expect the weight and token indices are aligned with balances returned from EVM query.
        // If necessary we would also pass the tokens along with the query result,
        // use them and fetch the weights from the registry by token address.
        for (i, balance) in balances.into_iter().enumerate() {
            reserves.insert(
                pool_data.tokens[i],
                TokenState {
                    balance,
                    scaling_exponent: pool_data.scaling_exponents[i],
                },
            );
        }
        StablePool {
            pool_id: pool_data.pool_id,
            pool_address: pool_data.pool_address,
            swap_fee_percentage,
            amplification_parameter,
            reserves,
            paused,
        }
    }
}

#[mockall::automock]
#[async_trait::async_trait]
pub trait BalancerPoolFetching: Send + Sync {
    async fn fetch(
        &self,
        token_pairs: HashSet<TokenPair>,
        at_block: Block,
    ) -> Result<Vec<BalancerPool>>;
}

pub struct BalancerPoolFetcher {
    pool_registry: Arc<BalancerPoolRegistry>,
    pool_reserve_cache: BalancerPoolReserveCache,
}

impl BalancerPoolFetcher {
    pub async fn new(
        chain_id: u64,
        web3: Web3,
        token_info_fetcher: Arc<dyn TokenInfoFetching>,
        config: CacheConfig,
        block_stream: CurrentBlockStream,
        metrics: Arc<dyn BalancerPoolCacheMetrics>,
        client: Client,
    ) -> Result<Self> {
        let pool_info = Arc::new(PoolInfoFetcher {
            web3: web3.clone(),
            token_info_fetcher: token_info_fetcher.clone(),
        });
        let pool_initializer = DefaultPoolInitializer::new(chain_id, pool_info.clone(), client)?;
        let pool_registry =
            Arc::new(BalancerPoolRegistry::new(web3.clone(), pool_initializer, pool_info).await?);
        let reserve_fetcher = PoolReserveFetcher::new(pool_registry.clone(), web3).await?;
        let pool_reserve_cache =
            RecentBlockCache::new(config, reserve_fetcher, block_stream, metrics)?;
        Ok(Self {
            pool_registry,
            pool_reserve_cache,
        })
    }
}

#[async_trait::async_trait]
impl BalancerPoolFetching for BalancerPoolFetcher {
    async fn fetch(
        &self,
        token_pairs: HashSet<TokenPair>,
        at_block: Block,
    ) -> Result<Vec<BalancerPool>> {
        let pool_ids = self
            .pool_registry
            .get_pool_ids_containing_token_pairs(token_pairs)
            .await;
        let fetched_pools = self.pool_reserve_cache.fetch(pool_ids, at_block).await?;
        // Return only those pools which are not paused.
        Ok(filter_paused(fetched_pools))
    }
}

fn filter_paused(pools: Vec<BalancerPool>) -> Vec<BalancerPool> {
    pools.into_iter().filter(|pool| !pool.paused()).collect()
}

#[async_trait::async_trait]
impl Maintaining for BalancerPoolFetcher {
    async fn run_maintenance(&self) -> Result<()> {
        futures::try_join!(
            self.pool_registry.run_maintenance(),
            self.pool_reserve_cache.update_cache(),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_paused_pools() {
        let pools = vec![
            BalancerPool::Weighted(WeightedPool {
                pool_id: H256::from_low_u64_be(0),
                pool_address: Default::default(),
                swap_fee_percentage: Bfp::zero(),
                reserves: Default::default(),
                paused: true,
            }),
            BalancerPool::Stable(StablePool {
                pool_id: H256::from_low_u64_be(1),
                pool_address: Default::default(),
                swap_fee_percentage: Bfp::zero(),
                amplification_parameter: Default::default(),
                reserves: Default::default(),
                paused: false,
            }),
        ];

        let filtered_pools = filter_paused(pools.clone());
        assert_eq!(filtered_pools.len(), 1);
        assert_eq!(filtered_pools[0].pool_id(), pools[1].pool_id());
    }

    #[test]
    fn is_weighted_() {
        let weighted_pool = BalancerPool::Weighted(WeightedPool {
            pool_id: H256::from_low_u64_be(0),
            pool_address: Default::default(),
            swap_fee_percentage: Bfp::zero(),
            reserves: Default::default(),
            paused: true,
        });
        let stable_pool = BalancerPool::Stable(StablePool {
            pool_id: H256::from_low_u64_be(1),
            pool_address: Default::default(),
            swap_fee_percentage: Bfp::zero(),
            amplification_parameter: Default::default(),
            reserves: Default::default(),
            paused: false,
        });

        assert!(weighted_pool.is_weighted());
        assert!(!stable_pool.is_weighted());
    }

    #[test]
    fn try_into_stable_and_weighted() {
        let weighted_pool = BalancerPool::Weighted(WeightedPool {
            pool_id: H256::from_low_u64_be(0),
            pool_address: Default::default(),
            swap_fee_percentage: Bfp::zero(),
            reserves: Default::default(),
            paused: true,
        });

        assert!(weighted_pool.try_into_weighted().is_ok());
        assert!(weighted_pool.try_into_stable().is_err());

        let stable_pool = BalancerPool::Stable(StablePool {
            pool_id: H256::from_low_u64_be(1),
            pool_address: Default::default(),
            swap_fee_percentage: Bfp::zero(),
            amplification_parameter: Default::default(),
            reserves: Default::default(),
            paused: false,
        });

        assert!(stable_pool.try_into_stable().is_ok());
        assert!(stable_pool.try_into_weighted().is_err());
    }
}
