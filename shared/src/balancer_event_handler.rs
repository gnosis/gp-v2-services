use crate::{
    current_block::BlockRetrieving,
    event_handling::{BlockNumber, EventHandler, EventIndex, EventStoring},
    impl_event_retrieving,
    maintenance::Maintaining,
};
use anyhow::{anyhow, Context, Result};
use contracts::{
    balancer_v2_vault::{
        self,
        event_data::{
            PoolRegistered as ContractPoolRegistered, TokensRegistered as ContractTokensRegistered,
        },
        Event as ContractEvent,
    },
    BalancerV2Vault,
};
use ethcontract::common::DeploymentInformation;
use ethcontract::{dyns::DynWeb3, Event as EthContractEvent, EventMetadata, H160, H256};
use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    ops::RangeInclusive,
};
use tokio::sync::Mutex;

#[derive(Debug)]
pub enum BalancerEvent {
    PoolRegistered(PoolRegistered),
    TokensRegistered(TokensRegistered),
}

#[derive(Debug)]
pub struct PoolRegistered {
    pub pool_id: H256,
    pub pool_address: H160,
    pub specialization: PoolSpecialization,
}

#[derive(Debug)]
pub struct TokensRegistered {
    pub pool_id: H256,
    pub tokens: Vec<H160>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct WeightedPool {
    pool_id: H256,
    pool_address: Option<H160>,
    tokens: Option<Vec<H160>>,
    block_created: u64,
}

#[derive(Debug, Default)]
pub struct BalancerPools {
    pools_by_token: HashMap<H160, HashSet<H256>>,
    pools: HashMap<H256, WeightedPool>,
    contract_deployment_block: u64,
}

/// There are three specialization settings for Pools,
/// which allow for cheaper swaps at the cost of reduced functionality:
#[derive(Debug)]
#[repr(u8)]
pub enum PoolSpecialization {
    /// no specialization, suited for all Pools. IGeneralPool is used for swap request callbacks,
    /// passing the balance of all tokens in the Pool. These Pools have the largest swap costs
    /// (because of the extra storage reads), which increase with the number of registered tokens.
    General = 0,
    /// IMinimalSwapInfoPool is used instead of IGeneralPool, which saves gas by only passing the
    /// balance of the two tokens involved in the swap. This is suitable for some pricing algorithms,
    /// like the weighted constant product one popularized by Balancer V1. Swap costs are
    /// smaller compared to general Pools, and are independent of the number of registered tokens.
    MinimalSwapInfo = 1,
    /// only allows two tokens to be registered. This achieves the lowest possible swap gas cost.
    /// Like minimal swap info Pools, these are called via IMinimalSwapInfoPool.
    TwoToken = 2,
}

impl PoolSpecialization {
    fn new(specialization: u8) -> Result<Self> {
        match specialization {
            0 => Ok(Self::General),
            1 => Ok(Self::MinimalSwapInfo),
            2 => Ok(Self::TwoToken),
            t => Err(anyhow!("Invalid PoolSpecialization value {} (> 2)", t)),
        }
    }
}

impl BalancerPools {

    // All insertions happen in one transaction.
    fn insert_events(&mut self, events: Vec<(EventIndex, BalancerEvent)>) -> Result<()> {
        for (index, event) in events {
            match event {
                BalancerEvent::PoolRegistered(event) => self.insert_pool(index, event),
                BalancerEvent::TokensRegistered(event) => self.include_token_data(index, event),
            };
        }
        Ok(())
    }

    fn delete_pools(&mut self, delete_from_block_number: u64) -> Result<()> {
        let pool_ids_to_delete = self
            .pools
            .iter()
            .filter_map(|(pool_id, pool)| {
                if pool.block_created >= delete_from_block_number {
                    return Some(*pool_id);
                }
                None
            })
            .collect::<HashSet<H256>>();

        self.pools
            .retain(|pool_id, _| !pool_ids_to_delete.contains(pool_id));
        // Remove all deleted pool_ids from token listing.
        // Note that this could result in an empty set for some tokens.
        for (_, pool_set) in self.pools_by_token.iter_mut() {
            *pool_set = pool_set
                .difference(&pool_ids_to_delete)
                .cloned()
                .collect::<HashSet<H256>>();
        }
        Ok(())
    }

    fn replace_events(
        &mut self,
        delete_from_block_number: u64,
        events: Vec<(EventIndex, BalancerEvent)>,
    ) -> Result<()> {
        self.delete_events(delete_from_block_number)?;
        self.insert_events(events)?;
        Ok(())
    }

    fn insert_pool(&mut self, index: EventIndex, registration: PoolRegistered) {
        match self.pools.get(&registration.pool_id) {
            None => {
                // PoolRegistered event is first to be processed, we leave tokens empty
                // and update when TokenRegistration event is processed.
                let weighted_pool = WeightedPool {
                    pool_id: registration.pool_id,
                    pool_address: Some(registration.pool_address),
                    // Tokens not emitted with registration event.
                    tokens: None,
                    block_created: index.block_number,
                };
                self.pools.insert(registration.pool_id, weighted_pool);
                tracing::debug!(
                    "Balancer Pool created from PoolRegistration {:?} (tokens pending...)",
                    registration
                );
            }
            Some(existing_pool) => {
                // If exists before PoolRegistration, then only pool_id and tokens known
                let tokens = existing_pool.clone().tokens;
                self.pools.insert(
                    registration.pool_id,
                    WeightedPool {
                        pool_id: registration.pool_id,
                        pool_address: Some(registration.pool_address),
                        tokens,
                        // Pool and Token Registration always occur in the same tx!
                        // https://github.com/balancer-labs/balancer-v2-monorepo/blob/70843e6a61ad11208c1cfabf5cfe15be216ca8d3/pkg/pool-utils/contracts/BasePool.sol#L128-L130
                        block_created: index.block_number,
                    },
                );
                tracing::debug!("Pool Address recovered for existing Balancer pool")
            }
        }
    }

    fn include_token_data(&mut self, index: EventIndex, registration: TokensRegistered) {
        let pool_id = registration.pool_id;
        let tokens = registration.tokens;
        match self.pools.get(&pool_id) {
            None => {
                // TokensRegistered event received before PoolRegistered, leaving pool_address
                // empty until PoolRegistration event is processed.
                let weighted_pool = WeightedPool {
                    pool_id,
                    // Pool address not emitted with registration event.
                    pool_address: None,
                    tokens: Some(tokens.clone()),
                    block_created: index.block_number,
                };
                self.pools.insert(pool_id, weighted_pool);
                tracing::debug!(
                    "Balancer Pool created from TokensRegistration (pool address pending...)",
                );
            }
            Some(existing_pool) => {
                // If pool exists before token registration, then only pool_id and address known
                let pool_address = existing_pool.clone().pool_address;
                self.pools.insert(
                    pool_id,
                    WeightedPool {
                        pool_id,
                        pool_address,
                        tokens: Some(tokens.clone()),
                        // Pool and Token Registration always occur in the same tx!
                        // https://github.com/balancer-labs/balancer-v2-monorepo/blob/70843e6a61ad11208c1cfabf5cfe15be216ca8d3/pkg/pool-utils/contracts/BasePool.sol#L128-L130
                        block_created: index.block_number,
                    },
                );
                tracing::debug!("Pool Tokens received for existing Balancer pool")
            }
        }
        // In either of the above cases we can now populate pools_by_token
        for token in tokens {
            self.pools_by_token
                .entry(token)
                .or_default()
                .insert(pool_id);
        }
    }

    fn contract_to_balancer_events(
        &self,
        contract_events: Vec<EthContractEvent<ContractEvent>>,
    ) -> Result<Vec<(EventIndex, BalancerEvent)>> {
        contract_events
            .into_iter()
            .filter_map(|EthContractEvent { data, meta }| {
                let meta = match meta {
                    Some(meta) => meta,
                    None => return Some(Err(anyhow!("event without metadata"))),
                };
                match data {
                    ContractEvent::PoolRegistered(event) => {
                        // Technically this is only needed for pool_address
                        Some(convert_pool_registered(&event, &meta))
                    }
                    ContractEvent::TokensRegistered(event) => {
                        Some(convert_tokens_registered(&event, &meta))
                    }
                    // ContractEvent::TokensDeregistered(event) => {
                    //     tracing::debug!("Tokens Deregistered {:?}", event);
                    //     None
                    // }
                    _ => {
                        // TODO - Not processing other events at the moment.
                        None
                    }
                }
            })
            .collect::<Result<Vec<_>>>()
    }
}

// pub struct BalancerPoolFetcher {
//     pub web3: Web3,
// }
//
// impl BalancerPoolFetcher {
//     async fn _get_pool_tokens(&self, _pool_address: H160) -> Vec<H160> {
//         // let web3 = Web3::new(self.web3.transport().clone());
//         // let pool_contract = BalancerPool::at(&web3, pool_address).await;
//         // TODO - fetch details from pool
//         // There are two different types of pools, hopefully they share a common interface.
//         vec![]
//     }
// }

pub struct BalancerEventUpdater(
    Mutex<EventHandler<DynWeb3, BalancerV2VaultContract, BalancerPools>>,
);

impl BalancerEventUpdater {
    pub async fn new(contract: BalancerV2Vault, mut pools: BalancerPools) -> Self {
        let deployment_block = match contract.deployment_information() {
            Some(DeploymentInformation::BlockNumber(block_number)) => Some(block_number),
            Some(DeploymentInformation::TransactionHash(hash)) => {
                match contract
                    .raw_instance()
                    .web3()
                    .block_number_from_tx_hash(hash)
                    .await
                {
                    Ok(block_number) => Some(block_number),
                    Err(err) => {
                        tracing::warn!("no deployment block for hash {}: {}", hash, err);
                        None
                    }
                }
            }
            None => None,
        };
        // This minor update keeps deployment block fetching self contained to here.
        pools.deployment_block = deployment_block.unwrap_or(0);
        Self(Mutex::new(EventHandler::new(
            contract.raw_instance().web3(),
            BalancerV2VaultContract(contract),
            pools,
            deployment_block,
        )))
    }
}

#[async_trait::async_trait]
impl EventStoring<ContractEvent> for BalancerPools {
    async fn replace_events(
        &mut self,
        events: Vec<EthContractEvent<ContractEvent>>,
        range: RangeInclusive<BlockNumber>,
    ) -> Result<()> {
        let balancer_events = self
            .contract_to_balancer_events(events)
            .context("failed to convert events")?;
        tracing::debug!(
            "replacing {} events from block number {}",
            balancer_events.len(),
            range.start().to_u64()
        );
        // Not sure we will event need to replace events as we don't store the events themselves.
        BalancerPools::replace_events(self, 0, balancer_events)?;
        Ok(())
    }

    async fn append_events(&mut self, events: Vec<EthContractEvent<ContractEvent>>) -> Result<()> {
        let balancer_events = self
            .contract_to_balancer_events(events)
            .context("failed to convert events")?;
        self.insert_events(balancer_events)
    }

    async fn last_event_block(&self) -> Result<u64> {
        // Technically we could keep this updated more effectively in a field on balancer pools,
        // but the maintenance seems like more overhead that needs to be tested.
        Ok(self
            .pools
            .iter()
            .map(|(_, pool)| pool.block_created)
            .max()
            .unwrap_or(self.contract_deployment_block))
    }
}

impl_event_retrieving! {
    pub BalancerV2VaultContract for balancer_v2_vault
}

#[async_trait::async_trait]
impl Maintaining for BalancerEventUpdater {
    async fn run_maintenance(&self) -> Result<()> {
        self.0.run_maintenance().await
    }
}

fn convert_pool_registered(
    registration: &ContractPoolRegistered,
    meta: &EventMetadata,
) -> Result<(EventIndex, BalancerEvent)> {
    let event = PoolRegistered {
        pool_id: H256::from(registration.pool_id),
        pool_address: registration.pool_address,
        specialization: PoolSpecialization::new(registration.specialization)?,
    };
    Ok((EventIndex::from(meta), BalancerEvent::PoolRegistered(event)))
}

fn convert_tokens_registered(
    registration: &ContractTokensRegistered,
    meta: &EventMetadata,
) -> Result<(EventIndex, BalancerEvent)> {
    let event = TokensRegistered {
        pool_id: H256::from(registration.pool_id),
        tokens: registration.tokens.clone(),
    };
    Ok((
        EventIndex::from(meta),
        BalancerEvent::TokensRegistered(event),
    ))
}
