//! For a given collection of `TokenPair`, returns all known  `RegisteredWeightedPools` from `BalancerPoolRegistry`
//! This does not come along with reserves or other block-sensitive data.
use model::TokenPair;
use std::collections::HashSet;

use crate::balancer::{
    event_handler::BalancerPoolRegistry,
    pool_storage::RegisteredWeightedPool,
};

#[async_trait::async_trait]
pub trait RegisteredWeightedPoolFetching: Send + Sync {
    async fn fetch(
        &self,
        token_pairs: HashSet<TokenPair>,
    ) -> Vec<RegisteredWeightedPool>;
}

pub struct RegisteredPoolFetcher {
    pool_registry: BalancerPoolRegistry,
}

#[async_trait::async_trait]
impl RegisteredWeightedPoolFetching for RegisteredPoolFetcher {
    async fn fetch(
        &self,
        token_pairs: HashSet<TokenPair>,
    ) -> Vec<RegisteredWeightedPool> {
        self
            .pool_registry
            .get_pools_containing_token_pairs(token_pairs)
            .await
    }
}
