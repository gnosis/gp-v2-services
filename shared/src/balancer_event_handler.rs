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

#[derive(Clone, Debug)]
pub enum BalancerEvent {
    PoolRegistered(PoolRegistered),
    TokensRegistered(TokensRegistered),
}

#[derive(Debug, Clone)]
pub struct PoolRegistered {
    pub pool_id: H256,
    pub pool_address: H160,
    pub specialization: PoolSpecialization,
}

#[derive(Debug, Clone)]
pub struct TokensRegistered {
    pub pool_id: H256,
    pub tokens: Vec<H160>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct WeightedPool {
    pool_id: H256,
    pool_address: Option<H160>,
    tokens: Option<Vec<H160>>,
    specialization: Option<PoolSpecialization>,
    block_created: u64,
}

impl WeightedPool {
    fn update_from_event(&mut self, event: BalancerEvent) {
        // Pool and Token Registration always occur in the same tx! So block_created doesn't need update.
        // https://github.com/balancer-labs/balancer-v2-monorepo/blob/70843e6a61ad11208c1cfabf5cfe15be216ca8d3/pkg/pool-utils/contracts/BasePool.sol#L128-L130
        match event {
            BalancerEvent::PoolRegistered(pool_registration) => {
                self.pool_address = Some(pool_registration.pool_address);
                self.specialization = Some(pool_registration.specialization);
            }
            BalancerEvent::TokensRegistered(token_registration) => {
                self.tokens = Some(token_registration.tokens)
            }
        }
    }
}

impl From<(EventIndex, PoolRegistered)> for WeightedPool {
    fn from(event_data: (EventIndex, PoolRegistered)) -> Self {
        Self {
            pool_id: event_data.1.pool_id,
            pool_address: Some(event_data.1.pool_address),
            specialization: Some(event_data.1.specialization),
            // Tokens not emitted with pool registration event.
            tokens: None,
            block_created: event_data.0.block_number,
        }
    }
}

impl From<(EventIndex, TokensRegistered)> for WeightedPool {
    fn from(event_data: (EventIndex, TokensRegistered)) -> Self {
        Self {
            pool_id: event_data.1.pool_id,
            // pool_address and specialization not emitted with token registration event.
            pool_address: None,
            specialization: None,
            tokens: Some(event_data.1.tokens),
            block_created: event_data.0.block_number,
        }
    }
}

#[derive(Debug, Default)]
pub struct BalancerPools {
    pools_by_token: HashMap<H160, HashSet<H256>>,
    pools: HashMap<H256, WeightedPool>,
    contract_deployment_block: u64,
}

/// There are three specialization settings for Pools,
/// which allow for cheaper swaps at the cost of reduced functionality:
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
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
                BalancerEvent::TokensRegistered(event) => self.insert_token_data(index, event),
            };
        }
        Ok(())
    }

    fn insert_pool(&mut self, index: EventIndex, registration: PoolRegistered) {
        match self.pools.get_mut(&registration.pool_id) {
            None => {
                // PoolRegistered event is first to be processed, we leave tokens empty
                // and update when TokenRegistration event is processed.
                tracing::debug!(
                    "Creating Balancer Pool with id {:?} from PoolRegistration event (tokens pending...)",
                    &registration.pool_id
                );
                self.pools.insert(
                    registration.pool_id,
                    WeightedPool::from((index, registration)),
                );
            }
            Some(existing_pool) => {
                // If exists before PoolRegistration, then only pool_id and tokens are known
                existing_pool.update_from_event(BalancerEvent::PoolRegistered(registration));
                tracing::debug!(
                    "Pool Address and specialization recovered for existing Balancer pool with id {:}", existing_pool.pool_id
                );
            }
        }
    }

    fn insert_token_data(&mut self, index: EventIndex, registration: TokensRegistered) {
        let pool_id = &registration.pool_id;
        match self.pools.get_mut(pool_id) {
            None => {
                // TokensRegistered event received before PoolRegistered, leaving pool_address
                // empty until PoolRegistration event is processed.
                tracing::debug!(
                    "Creating Balancer Pool with id {:?} from TokensRegistration event (pool address pending...)", pool_id
                );
                self.pools
                    .insert(*pool_id, WeightedPool::from((index, registration.clone())));
            }
            Some(existing_pool) => {
                // If pool exists before token registration, then only pool_id and address known
                existing_pool
                    .update_from_event(BalancerEvent::TokensRegistered(registration.clone()));
                tracing::debug!(
                    "Pool Tokens recovered for existing Balancer pool with id {:?}",
                    pool_id
                )
            }
        }

        // In either of the above cases we can now populate pools_by_token
        for token in registration.tokens {
            self.pools_by_token
                .entry(token)
                .or_default()
                .insert(*pool_id);
        }
    }

    fn replace_events(
        &mut self,
        delete_from_block_number: u64,
        events: Vec<(EventIndex, BalancerEvent)>,
    ) -> Result<()> {
        self.delete_pools(delete_from_block_number)?;
        self.insert_events(events)?;
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

    fn last_event_block(&self) -> u64 {
        // Technically we could keep this updated more effectively in a field on balancer pools,
        // but the maintenance seems like more overhead that needs to be tested.
        self.pools
            .iter()
            .map(|(_, pool)| pool.block_created)
            .max()
            .unwrap_or(self.contract_deployment_block)
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
        pools.contract_deployment_block = deployment_block.unwrap_or(0);
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
        Ok(self.last_event_block())
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
        pool_id: H256::from(registration.pool_id.0),
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
        pool_id: H256::from(registration.pool_id.0),
        tokens: registration.tokens.clone(),
    };
    Ok((
        EventIndex::from(meta),
        BalancerEvent::TokensRegistered(event),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use maplit::{hashmap, hashset};

    #[test]
    fn balancer_insert_pool() {
        let mut pool_store = BalancerPools::default();
        let index = EventIndex {
            block_number: 1,
            log_index: 0,
        };
        let pool_id = H256::from_low_u64_be(1);
        let pool_address = H160::from_low_u64_be(1);
        let specialization = PoolSpecialization::General;

        let registration = PoolRegistered {
            pool_id,
            pool_address,
            specialization,
        };

        pool_store.insert_pool(index, registration);
        let expected = hashmap! {H256::from_low_u64_be(1) => WeightedPool {
            pool_id,
            pool_address: Some(pool_address),
            specialization: Some(specialization),
            tokens: None,
            block_created: 1,
        }};
        assert_eq!(pool_store.pools, expected);
        assert_eq!(pool_store.pools_by_token, HashMap::new());

        // Branch where token registration event already stored
        let tokens = vec![H160::from_low_u64_be(2), H160::from_low_u64_be(3)];
        let mut weighted_pool = WeightedPool {
            pool_id,
            pool_address: None,
            specialization: None,
            tokens: Some(tokens.clone()),
            block_created: 1,
        };
        let mut pools = hashmap! {H256::from_low_u64_be(1) => weighted_pool.clone() };
        let pools_by_token = hashmap! {
            tokens[0] => hashset! { pool_id },
            tokens[1] => hashset! { pool_id },
        };
        let mut pool_store = BalancerPools {
            pools_by_token: pools_by_token.clone(),
            pools: pools.clone(),
            contract_deployment_block: 0,
        };

        let registration = PoolRegistered {
            pool_id,
            pool_address,
            specialization,
        };

        pool_store.insert_pool(index, registration);
        weighted_pool.pool_address = Some(pool_address);
        weighted_pool.specialization = Some(specialization);
        *pools.get_mut(&pool_id).unwrap() = weighted_pool;
        assert_eq!(pool_store.pools, pools);
        assert_eq!(pool_store.pools_by_token, pools_by_token);
    }

    #[test]
    fn balancer_insert_token_data() {
        let mut pool_store = BalancerPools::default();
        let index = EventIndex {
            block_number: 1,
            log_index: 0,
        };
        let pool_id = H256::from_low_u64_be(1);
        let tokens = vec![H160::from_low_u64_be(2), H160::from_low_u64_be(3)];

        let registration = TokensRegistered {
            pool_id,
            tokens: tokens.clone(),
        };

        pool_store.insert_token_data(index, registration);
        let expected_pool_map = hashmap! {H256::from_low_u64_be(1) => WeightedPool {
            pool_id,
            pool_address: None,
            specialization: None,
            tokens: Some(tokens.clone()),
            block_created: 1,
        }};
        let expected_token_map = hashmap! {
            tokens[0] => hashset! { pool_id },
            tokens[1] => hashset! { pool_id },
        };
        assert_eq!(pool_store.pools, expected_pool_map);
        assert_eq!(pool_store.pools_by_token, expected_token_map);

        // Branch where pool registered already stored
        let pool_address = H160::from_low_u64_be(1);
        let specialization = PoolSpecialization::General;
        let mut weighted_pool = WeightedPool {
            pool_id,
            pool_address: Some(pool_address),
            specialization: Some(specialization),
            tokens: None,
            block_created: 1,
        };
        let mut pools = hashmap! {H256::from_low_u64_be(1) => weighted_pool.clone() };
        let mut pools_by_token = HashMap::new();
        let mut pool_store = BalancerPools {
            pools_by_token: pools_by_token.clone(),
            pools: pools.clone(),
            contract_deployment_block: 0,
        };

        let registration = TokensRegistered {
            pool_id,
            tokens: tokens.clone(),
        };

        pool_store.insert_token_data(index, registration);

        // update expected state
        weighted_pool.tokens = Some(tokens.clone());
        *pools.get_mut(&pool_id).unwrap() = weighted_pool;
        pools_by_token.insert(tokens[0], hashset! { pool_id });
        pools_by_token.insert(tokens[1], hashset! { pool_id });

        assert_eq!(pool_store.pools, pools);
        assert_eq!(pool_store.pools_by_token, pools_by_token);
    }

    #[test]
    fn balancer_delete_pools() {
        // Construct a bunch of pools
        let n = 3;
        let tokens: Vec<H160> = (0..n + 1).map(H160::from_low_u64_be).collect();
        let mut pools = HashMap::new();
        let mut pools_by_token: HashMap<H160, HashSet<H256>> = HashMap::new();
        for i in 0..n {
            let pool_id = H256::from_low_u64_be(i + 1);
            let token_a = tokens[i as usize];
            let token_b = tokens[i as usize + 1];
            pools.insert(
                pool_id,
                WeightedPool {
                    pool_id,
                    tokens: Some(vec![token_a, token_b]),
                    block_created: i + 1,
                    // Pool Specialization isn't relevant here.
                    ..Default::default()
                },
            );
            pools_by_token.entry(token_a).or_default().insert(pool_id);
            pools_by_token.entry(token_b).or_default().insert(pool_id);
        }

        let mut pool_store = BalancerPools {
            pools_by_token,
            pools,
            contract_deployment_block: 0,
        };
        assert_eq!(pool_store.last_event_block(), 3);
        pool_store.delete_pools(3).unwrap();
        assert_eq!(pool_store.last_event_block(), 2);
        pool_store.delete_pools(2).unwrap();
        assert_eq!(pool_store.last_event_block(), 1);
        pool_store.delete_pools(1).unwrap();
        assert_eq!(pool_store.last_event_block(), 0);
    }

    #[test]
    fn balancer_insert_events() {
        let n = 3usize;
        let pool_ids: Vec<H256> = (0..n).map(|i|H256::from_low_u64_be(i as u64)).collect();
        let pool_addresses: Vec<H160> = (0..n).map(|i| H160::from_low_u64_be(i as u64)).collect();
        let tokens: Vec<H160> = (0..n + 1).map(|i | H160::from_low_u64_be(i as u64)).collect();
        let specializations: Vec<PoolSpecialization> = (0..n).map(|i| PoolSpecialization::new(i as u8 % 3).unwrap()).collect();
        let pool_registration_events: Vec<BalancerEvent> = (0..n).map(|i| {
            BalancerEvent::PoolRegistered(PoolRegistered {
                pool_id: pool_ids[i],
                pool_address: pool_addresses[i],
                specialization: specializations[i],
            })
        }).collect();
        let token_registration_events: Vec<BalancerEvent> = (0..n).map(|i| {
            BalancerEvent::TokensRegistered(TokensRegistered {
                pool_id: pool_ids[i],
                tokens: vec![tokens[i], tokens[i + 1]],
            })
        }).collect();

        let events: Vec<(EventIndex, BalancerEvent)> = vec![
            // Block 1 has both Pool and Tokens registered
            (EventIndex::new(1, 0), pool_registration_events[0].clone()),
            (EventIndex::new(1, 0), token_registration_events[0].clone()),
            // Next pool registered in block 1 with tokens only coming in block 2
            // Not realistic, but we can handle it.
            (EventIndex::new(1, 0), pool_registration_events[1].clone()),
            (EventIndex::new(2, 0), token_registration_events[1].clone()),
            // Next tokens registered in block 3, but corresponding pool not received till block 4
            (EventIndex::new(3, 0), token_registration_events[2].clone()),
            (EventIndex::new(4, 0), pool_registration_events[2].clone()),
        ];

        let mut pool_store = BalancerPools::default();
        pool_store.insert_events(events).unwrap();
        // Note that it is never expected that blocks for events will differ,
        // but in this test block_created for the pool is the first block it receives.
        assert_eq!(pool_store.last_event_block(), 3);
        assert_eq!(pool_store.pools_by_token.get(&tokens[0]).unwrap(), &hashset! { pool_ids[0] });
        assert_eq!(pool_store.pools_by_token.get(&tokens[1]).unwrap(), &hashset! { pool_ids[0], pool_ids[1] });
        assert_eq!(pool_store.pools_by_token.get(&tokens[2]).unwrap(), &hashset! { pool_ids[1], pool_ids[2] });
        assert_eq!(pool_store.pools_by_token.get(&tokens[3]).unwrap(), &hashset! { pool_ids[2] });

        assert_eq!(pool_store.pools.get(&pool_ids[0]).unwrap(), &WeightedPool {
            pool_id: pool_ids[0],
            pool_address: Some(pool_addresses[0]),
            tokens: Some(vec![tokens[0], tokens[1]]),
            specialization: Some(PoolSpecialization::new(0).unwrap()),
            block_created: 1
        });
        assert_eq!(pool_store.pools.get(&pool_ids[1]).unwrap(), &WeightedPool {
            pool_id: pool_ids[1],
            pool_address: Some(pool_addresses[1]),
            tokens: Some(vec![tokens[1], tokens[2]]),
            specialization: Some(PoolSpecialization::new(1).unwrap()),
            block_created: 1
        });
        assert_eq!(pool_store.pools.get(&pool_ids[2]).unwrap(), &WeightedPool {
            pool_id: pool_ids[2],
            pool_address: Some(pool_addresses[2]),
            tokens: Some(vec![tokens[2], tokens[3]]),
            specialization: Some(PoolSpecialization::new(2).unwrap()),
            block_created: 3
        });
    }
}
