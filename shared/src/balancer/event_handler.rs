use crate::balancer::{
    info_fetching::PoolInfoFetcher,
    pool_models::{PoolCreated, PoolStorage, RegisteredWeightedPool},
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
use ethcontract::common::DeploymentInformation;
use ethcontract::Event as EthContractEvent;
use model::TokenPair;
use std::sync::Arc;
use std::{collections::HashSet, ops::RangeInclusive};
use tokio::sync::Mutex;

pub struct BalancerPoolRegistry {
    weighted_pool_updater:
        Mutex<EventHandler<Web3, BalancerV2WeightedPoolFactoryContract, PoolStorage>>,
    two_token_pool_updater:
        Mutex<EventHandler<Web3, BalancerV2WeightedPool2TokensFactoryContract, PoolStorage>>,
}

impl BalancerPoolRegistry {
    pub async fn new(web3: Web3, token_info_fetcher: Arc<dyn TokenInfoFetching>) -> Result<Self> {
        let weighted_pool_factory = BalancerV2WeightedPoolFactory::deployed(&web3).await?;
        let two_token_pool_factory = BalancerV2WeightedPool2TokensFactory::deployed(&web3).await?;
        let deployment_block_weighted_pool =
            get_deployment_block(weighted_pool_factory.deployment_information(), &web3).await;
        let deployment_block_two_token_pool =
            get_deployment_block(two_token_pool_factory.deployment_information(), &web3).await;
        let weighted_pool_updater = Mutex::new(EventHandler::new(
            web3.clone(),
            BalancerV2WeightedPoolFactoryContract(weighted_pool_factory),
            PoolStorage::new(Box::new(PoolInfoFetcher {
                web3: web3.clone(),
                token_info_fetcher: token_info_fetcher.clone(),
            })),
            deployment_block_weighted_pool,
        ));
        let two_token_pool_updater = Mutex::new(EventHandler::new(
            web3.clone(),
            BalancerV2WeightedPool2TokensFactoryContract(two_token_pool_factory),
            PoolStorage::new(Box::new(PoolInfoFetcher {
                web3,
                token_info_fetcher,
            })),
            deployment_block_two_token_pool,
        ));
        Ok(Self {
            weighted_pool_updater,
            two_token_pool_updater,
        })
    }

    pub async fn get_pools_containing_token_pairs(
        &self,
        token_pairs: HashSet<TokenPair>,
    ) -> Vec<RegisteredWeightedPool> {
        let mut pool_set_1 = self
            .weighted_pool_updater
            .lock()
            .await
            .store
            .pools_containing_token_pairs(token_pairs.clone());
        let pool_set_2 = self
            .two_token_pool_updater
            .lock()
            .await
            .store
            .pools_containing_token_pairs(token_pairs);
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
        PoolStorage::replace_events_inner(
            self,
            range.start().to_u64(),
            convert_weighted_pool_created(events)?,
        )
        .await
    }

    async fn append_events(
        &mut self,
        events: Vec<EthContractEvent<WeightedPoolFactoryEvent>>,
    ) -> Result<()> {
        self.insert_events(convert_weighted_pool_created(events)?)
            .await
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
        PoolStorage::replace_events_inner(
            self,
            range.start().to_u64(),
            convert_two_token_pool_created(events)?,
        )
        .await
    }

    async fn append_events(
        &mut self,
        events: Vec<EthContractEvent<WeightedPool2TokensFactoryEvent>>,
    ) -> Result<()> {
        self.insert_events(convert_two_token_pool_created(events)?)
            .await
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
        futures::try_join!(
            self.two_token_pool_updater.run_maintenance(),
            self.weighted_pool_updater.run_maintenance(),
        )?;
        Ok(())
    }
}

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
