//! This event handler contains mostly boiler plate code for the implementation of `EventRetrieving`
//! and `EventStoring` for Balancer Pool Factory contracts and `PoolStorage` respectively.
//! Because there are multiple factory contracts for which we rely on event data, the
//! `BalancerPoolRegistry` is responsible for multiple EventHandlers.
//!
//! Apart from the event handling boiler plate, there are a few helper methods used as adapters
//! for converting received contract event data into appropriate internal structs to be passed
//! along to the `PoolStorage` (database) for update
//!
//! Due to limitations of `EventRetrieving` we must put each event handler behind its own Mutex.
//! - These mutexes are locked during synchronization and pool fetching.
//!
//! *Note that* when loading pool from a cold start synchronization can take quite long, but is
//! otherwise as quick as possible (i.e. taking advantage of as much cached information as possible).
use crate::balancer::{
    info_fetching::PoolInfoFetcher,
    pool_storage::{PoolCreated, PoolStorage, RegisteredWeightedPool},
};
use crate::token_info::TokenInfoFetching;
use crate::{
    current_block::BlockRetrieving,
    event_handling::{BlockNumber, EventHandler, EventIndex, EventStoring},
    impl_event_retrieving,
    maintenance::Maintaining,
    Web3,
};
use anyhow::{anyhow, Context, Result};
use contracts::{
    balancer_v2_weighted_pool_2_tokens_factory::{self, Event as WeightedPool2TokensFactoryEvent},
    balancer_v2_weighted_pool_factory::{self, Event as WeightedPoolFactoryEvent},
    BalancerV2WeightedPool2TokensFactory, BalancerV2WeightedPoolFactory,
};
use ethcontract::{common::DeploymentInformation, Event as EthContractEvent, H256};
use model::TokenPair;
use std::sync::Arc;
use std::{collections::HashSet, ops::RangeInclusive};
use tokio::sync::Mutex;

/// The Pool Registry maintains an event handler for each of the Balancer Pool Factory contracts
/// and maintains a `PoolStorage` for each.
/// Pools are read from this registry, via the public method `get_pool_ids_containing_token_pairs`
/// which takes a collection of `TokenPair`, gets the relevant pools from each `PoolStorage`
/// and returns a merged de-duplicated version of the results.
pub struct BalancerPoolRegistry {
    weighted_pool_updater:
        Mutex<EventHandler<Web3, BalancerV2WeightedPoolFactoryContract, PoolStorage>>,
    two_token_pool_updater:
        Mutex<EventHandler<Web3, BalancerV2WeightedPool2TokensFactoryContract, PoolStorage>>,
}

impl BalancerPoolRegistry {
    /// Deployed Pool Factories are loaded internally from the provided `web3` which is also used
    /// together with `token_info_fetcher` to construct a `PoolInfoFetcher` for each Event Handler.
    pub async fn new(web3: Web3, token_info_fetcher: Arc<dyn TokenInfoFetching>) -> Result<Self> {
        let weighted_pool_factory = BalancerV2WeightedPoolFactory::deployed(&web3).await?;
        let two_token_pool_factory = BalancerV2WeightedPool2TokensFactory::deployed(&web3).await?;
        let deployment_block_weighted_pool =
            get_deployment_block(weighted_pool_factory.deployment_information(), &web3).await;
        let deployment_block_two_token_pool =
            get_deployment_block(two_token_pool_factory.deployment_information(), &web3).await;
        let weighted_pool_registry = PoolStorage::new_with_data(Box::new(PoolInfoFetcher {
            web3: web3.clone(),
            token_info_fetcher: token_info_fetcher.clone(),
        })).await;
        let weighted_pool_updater = Mutex::new(EventHandler::new(
            web3.clone(),
            BalancerV2WeightedPoolFactoryContract(weighted_pool_factory),
            weighted_pool_registry.0,
            Some(weighted_pool_registry.1),
        ));
        let two_token_pool_updater = Mutex::new(EventHandler::new(
            web3.clone(),
            BalancerV2WeightedPool2TokensFactoryContract(two_token_pool_factory),
            PoolStorage::new(Box::new(PoolInfoFetcher {
                web3,
                token_info_fetcher,
            })),
            Some(weighted_pool_registry.1),
        ));
        Ok(Self {
            weighted_pool_updater,
            two_token_pool_updater,
        })
    }

    /// Retrieves `RegisteredWeightedPool`s from each Pool Store in the Registry and
    /// returns the merged result.
    /// Primarily intended to be used by `BalancerPoolFetcher`.
    pub async fn get_pool_ids_containing_token_pairs(
        &self,
        token_pairs: HashSet<TokenPair>,
    ) -> HashSet<H256> {
        let pool_set_1 = self
            .weighted_pool_updater
            .lock()
            .await
            .store
            .ids_for_pools_containing_token_pairs(token_pairs.clone());
        let pool_set_2 = self
            .two_token_pool_updater
            .lock()
            .await
            .store
            .ids_for_pools_containing_token_pairs(token_pairs);
        pool_set_1.union(&pool_set_2).copied().collect()
    }

    pub async fn get_pools(&self, pool_ids: &HashSet<H256>) -> Vec<RegisteredWeightedPool> {
        let mut pool_set_1 = self
            .weighted_pool_updater
            .lock()
            .await
            .store
            .pools_for(pool_ids);
        let pool_set_2 = self
            .two_token_pool_updater
            .lock()
            .await
            .store
            .pools_for(pool_ids);
        pool_set_1.extend(pool_set_2);
        pool_set_1
    }

    pub async fn get_all_pools(&self) -> Vec<RegisteredWeightedPool> {
        let mut pool_set_1 = self.weighted_pool_updater.lock().await.store.all_pools();
        let pool_set_2 = self.two_token_pool_updater.lock().await.store.all_pools();
        pool_set_1.extend(pool_set_2);
        pool_set_1
    }
}

async fn get_deployment_block(
    deployment_info: Option<DeploymentInformation>,
    web3: &Web3,
) -> Option<u64> {
    match deployment_info {
        Some(DeploymentInformation::BlockNumber(block_number)) => Some(block_number),
        Some(DeploymentInformation::TransactionHash(hash)) => {
            Some(web3.block_number_from_tx_hash(hash).await.ok()?)
        }
        None => None,
    }
}

#[async_trait::async_trait]
impl EventStoring<WeightedPoolFactoryEvent> for PoolStorage {
    async fn replace_events(
        &mut self,
        events: Vec<EthContractEvent<WeightedPoolFactoryEvent>>,
        range: RangeInclusive<BlockNumber>,
    ) -> Result<()> {
        self.replace_events_inner(
            range.start().to_u64(),
            convert_weighted_pool_created(events)?,
        )
        .await
    }

    async fn append_events(
        &mut self,
        events: Vec<EthContractEvent<WeightedPoolFactoryEvent>>,
    ) -> Result<()> {
        tracing::info!("inserting {} Weighted Pools from events", events.len());
        self.insert_events(convert_weighted_pool_created(events)?)
            .await?;
        tracing::info!(
            "now holding {} Weighted Pools upto block {}",
            self.all_pools().len(),
            self.last_event_block()
        );
        Ok(())
    }

    async fn last_event_block(&self) -> Result<u64> {
        Ok(self.last_event_block())
    }
}

#[async_trait::async_trait]
impl EventStoring<WeightedPool2TokensFactoryEvent> for PoolStorage {
    async fn replace_events(
        &mut self,
        events: Vec<EthContractEvent<WeightedPool2TokensFactoryEvent>>,
        range: RangeInclusive<BlockNumber>,
    ) -> Result<()> {
        self.replace_events_inner(
            range.start().to_u64(),
            convert_two_token_pool_created(events)?,
        )
        .await
    }

    async fn append_events(
        &mut self,
        events: Vec<EthContractEvent<WeightedPool2TokensFactoryEvent>>,
    ) -> Result<()> {
        tracing::info!(
            "Inserting {} Weighted 2-Token Pools from events",
            events.len()
        );
        self.insert_events(convert_two_token_pool_created(events)?)
            .await?;
        tracing::info!(
            "now holding {} 2-Token Weighted Pools upto block {}",
            self.all_pools().len(),
            self.last_event_block()
        );
        Ok(())
    }

    async fn last_event_block(&self) -> Result<u64> {
        Ok(self.last_event_block())
    }
}

impl_event_retrieving! {
    pub BalancerV2WeightedPoolFactoryContract for balancer_v2_weighted_pool_factory
}

impl_event_retrieving! {
    pub BalancerV2WeightedPool2TokensFactoryContract for balancer_v2_weighted_pool_2_tokens_factory
}

#[async_trait::async_trait]
impl Maintaining for BalancerPoolRegistry {
    async fn run_maintenance(&self) -> Result<()> {
        // Using try join here destroys all progress if one fails.
        self.two_token_pool_updater.run_maintenance().await?;
        tracing::info!("completed one round of 2-token pool sync");
        self.weighted_pool_updater.run_maintenance().await?;
        tracing::info!("completed one round of weighted pool sync");
        let all_pools = self.get_all_pools().await;
        let last_event = all_pools
            .iter()
            .map(|pool| pool.block_created)
            .max()
            .unwrap_or(0);
        tracing::info!(
            "all {} known balancer pools loaded! Last event block: {}",
            all_pools.len(),
            last_event
        );
        Ok(())
    }
}

/// Adapter methods for converting contract events from each pool factory into a single
/// `PoolCreated` struct that all event handlers are compatible with.
fn contract_to_pool_creation<T>(
    contract_events: Vec<EthContractEvent<T>>,
    adapter: impl Fn(T) -> PoolCreated,
) -> Result<Vec<(EventIndex, PoolCreated)>> {
    contract_events
        .into_iter()
        .map(|EthContractEvent { data, meta }| {
            let meta = meta.ok_or_else(|| anyhow!("event without metadata"))?;
            Ok((EventIndex::from(&meta), adapter(data)))
        })
        .collect::<Result<Vec<_>>>()
        .context("failed to convert events")
}

fn convert_weighted_pool_created(
    events: Vec<EthContractEvent<WeightedPoolFactoryEvent>>,
) -> Result<Vec<(EventIndex, PoolCreated)>> {
    contract_to_pool_creation(events, |event| match event {
        WeightedPoolFactoryEvent::PoolCreated(creation) => PoolCreated {
            pool_address: creation.pool,
        },
    })
}

fn convert_two_token_pool_created(
    events: Vec<EthContractEvent<WeightedPool2TokensFactoryEvent>>,
) -> Result<Vec<(EventIndex, PoolCreated)>> {
    contract_to_pool_creation(events, |event| match event {
        WeightedPool2TokensFactoryEvent::PoolCreated(creation) => PoolCreated {
            pool_address: creation.pool,
        },
    })
}
