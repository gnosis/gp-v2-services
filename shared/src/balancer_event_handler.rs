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
use std::convert::TryInto;
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

#[derive(Debug, Clone, PartialEq)]
pub struct PoolRegistered {
    pub pool_id: H256,
    pub pool_address: H160,
    pub specialization: PoolSpecialization,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TokensRegistered {
    pub pool_id: H256,
    pub tokens: Vec<H160>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct WeightedPool {
    pool_id: H256,
    pool_address: H160,
    tokens: Vec<H160>,
    specialization: PoolSpecialization,
    block_created: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WeightedPoolBuilder {
    pool_registration: Option<PoolRegistered>,
    tokens_registration: Option<TokensRegistered>,
    block_created: u64,
}

impl TryInto<WeightedPool> for WeightedPoolBuilder {
    type Error = ();

    fn try_into(self) -> Result<WeightedPool, Self::Error> {
        if self.pool_registration.is_some() && self.tokens_registration.is_some() {
            let pool_data = self.pool_registration.as_ref().unwrap();
            return Ok(WeightedPool {
                pool_id: pool_data.pool_id,
                pool_address: pool_data.pool_address,
                tokens: self.tokens_registration.clone().unwrap().tokens,
                specialization: pool_data.specialization,
                block_created: self.block_created,
            });
        }
        // TODO - make an error enum for this.
        Err(())
    }
}

/// The BalancerPool struct represents in-memory storage of all deployed Balancer Pools
#[derive(Debug, Default)]
pub struct BalancerPools {
    /// Used for O(1) access to all pool_ids for a given token
    pools_by_token: HashMap<H160, HashSet<H256>>,
    /// WeightedPool data for a given PoolId
    pools: HashMap<H256, WeightedPool>,
    /// Temporary storage for WeightedPools containing insufficient constructor data
    pending_pools: HashMap<H256, WeightedPoolBuilder>,
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
    fn try_upgrade(&mut self, pool_builder: WeightedPoolBuilder) {
        match pool_builder.clone().try_into() {
            Ok(weighted_pool) => {
                let token_registration = pool_builder.tokens_registration.expect("known to exist");
                // When upgradable, delete pending pool and add to valid pools
                let pool_id = token_registration.pool_id;
                tracing::info!("Upgrading Pool Builder with id {:?}", pool_id);
                self.pending_pools.remove(&pool_id);
                self.pools.insert(pool_id, weighted_pool);
                for token in token_registration.tokens {
                    self.pools_by_token
                        .entry(token)
                        .or_default()
                        .insert(pool_id);
                }
            }
            Err(_) => {
                tracing::info!("Pool Builder not yet upgradable");
            }
        }
    }

    // All insertions happen in one transaction.
    fn insert_events(&mut self, events: Vec<(EventIndex, BalancerEvent)>) -> Result<()> {
        for (index, event) in events {
            let pool_builder = match event {
                BalancerEvent::PoolRegistered(event) => self.insert_pool(index, event),
                BalancerEvent::TokensRegistered(event) => self.insert_token_data(index, event),
            };
            // In the future, when processing TokensDeregistered we may have to downgrade the result.
            self.try_upgrade(pool_builder)
        }
        Ok(())
    }

    fn insert_pool(
        &mut self,
        index: EventIndex,
        registration: PoolRegistered,
    ) -> WeightedPoolBuilder {
        let pool_builder =
            self.pending_pools
                .entry(registration.pool_id)
                .or_insert(WeightedPoolBuilder {
                    pool_registration: None,
                    tokens_registration: None,
                    block_created: index.block_number,
                });
        // Whether the entry was there already or not, we set PoolRegistered
        pool_builder.pool_registration = Some(registration);
        pool_builder.to_owned()
    }

    fn insert_token_data(
        &mut self,
        index: EventIndex,
        registration: TokensRegistered,
    ) -> WeightedPoolBuilder {
        let pool_builder =
            self.pending_pools
                .entry(registration.pool_id)
                .or_insert(WeightedPoolBuilder {
                    pool_registration: None,
                    tokens_registration: None,
                    block_created: index.block_number,
                });
        // Whether the entry was there already or not, we set TokensRegistered
        pool_builder.tokens_registration = Some(registration);
        pool_builder.to_owned()
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
        self.pools
            .retain(|_, pool| pool.block_created < delete_from_block_number);
        self.pending_pools
            .retain(|_, pool| pool.block_created < delete_from_block_number);
        // Note that this could result in an empty set for some tokens.
        let retained_pool_ids: HashSet<H256> = self.pools.keys().copied().collect();
        for (_, pool_set) in self.pools_by_token.iter_mut() {
            *pool_set = pool_set
                .intersection(&retained_pool_ids)
                .cloned()
                .collect::<HashSet<H256>>();
        }
        Ok(())
    }

    fn last_event_block(&self) -> u64 {
        // Technically we could keep this updated more effectively in a field on balancer pools,
        // but the maintenance seems like more overhead that needs to be tested.
        let pending_max = self
            .pending_pools
            .iter()
            .map(|(_, pool_builder)| pool_builder.block_created)
            .max()
            .unwrap_or(0);
        let pool_max = self
            .pools
            .iter()
            .map(|(_, pool)| pool.block_created)
            .max()
            .unwrap_or(0);
        pending_max.max(pool_max)
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
                        // https://github.com/gnosis/gp-v2-services/issues/681
                        None
                    }
                }
            })
            .collect::<Result<Vec<_>>>()
    }
}

pub struct BalancerEventUpdater(
    Mutex<EventHandler<DynWeb3, BalancerV2VaultContract, BalancerPools>>,
);

impl BalancerEventUpdater {
    pub async fn new(contract: BalancerV2Vault, pools: BalancerPools) -> Result<Self> {
        let deployment_block = match contract.deployment_information() {
            Some(DeploymentInformation::BlockNumber(block_number)) => Some(block_number),
            Some(DeploymentInformation::TransactionHash(hash)) => Some(
                contract
                    .raw_instance()
                    .web3()
                    .block_number_from_tx_hash(hash)
                    .await?,
            ),
            None => None,
        };
        Ok(Self(Mutex::new(EventHandler::new(
            contract.raw_instance().web3(),
            BalancerV2VaultContract(contract),
            pools,
            deployment_block,
        ))))
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

        pool_store.insert_pool(index, registration.clone());
        let expected = hashmap! {H256::from_low_u64_be(1) => WeightedPoolBuilder {
            pool_registration: Some(registration),
            tokens_registration: None,
            block_created: 1,
        }};
        assert_eq!(pool_store.pending_pools, expected);
        assert_eq!(pool_store.pools_by_token, HashMap::new());

        // Branch where token registration event already stored
        let mut pending_pools = hashmap! {H256::from_low_u64_be(1) => WeightedPoolBuilder {
            pool_registration: None,
            tokens_registration: Some(TokensRegistered {
                pool_id,
                tokens: vec![H160::from_low_u64_be(2), H160::from_low_u64_be(3)],
            }),
            block_created: 1,
        }};
        let mut pool_store = BalancerPools {
            pools_by_token: HashMap::new(),
            pending_pools: pending_pools.clone(),
            pools: HashMap::new(),
        };

        let registration = PoolRegistered {
            pool_id,
            pool_address,
            specialization,
        };

        pool_store.insert_pool(index, registration.clone());
        pending_pools.get_mut(&pool_id).unwrap().pool_registration = Some(registration);
        assert_eq!(pool_store.pending_pools, pending_pools);
        assert_eq!(pool_store.pools_by_token, HashMap::new());
        assert_eq!(pool_store.pools, HashMap::new());
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

        pool_store.insert_token_data(index, registration.clone());
        let expected_pending_pools = hashmap! {H256::from_low_u64_be(1) => WeightedPoolBuilder {
            pool_registration: None,
            tokens_registration: Some(registration),
            block_created: 1,
        }};
        assert_eq!(pool_store.pending_pools, expected_pending_pools);
        assert_eq!(pool_store.pools, HashMap::new());
        assert_eq!(pool_store.pools_by_token, HashMap::new());

        // Branch where pool registered already stored
        let pool_address = H160::from_low_u64_be(1);
        let specialization = PoolSpecialization::General;
        let mut pool_builder = WeightedPoolBuilder {
            pool_registration: Some(PoolRegistered {
                pool_id,
                pool_address,
                specialization,
            }),
            tokens_registration: None,
            block_created: 1,
        };
        let pending_pools = hashmap! {H256::from_low_u64_be(1) => pool_builder.clone() };
        let mut pools_by_token = HashMap::new();
        let mut pool_store = BalancerPools {
            pools_by_token: pools_by_token.clone(),
            pools: HashMap::new(),
            pending_pools,
        };

        let registration = TokensRegistered {
            pool_id,
            tokens: tokens.clone(),
        };

        pool_store.insert_token_data(index, registration.clone());

        // upgrade ready pending pool entry is still pending.
        pool_builder.tokens_registration = Some(registration);
        assert_eq!(pool_store.pools, HashMap::new());
        assert_eq!(
            pool_store.pending_pools,
            hashmap! { pool_id => pool_builder.clone() }
        );
        assert_eq!(pool_store.pools_by_token, HashMap::new());

        // The remaining assertions go a bit beyond scope of this test.
        // namely testing try_upgrade on success.
        pool_store.try_upgrade(pool_builder);

        // update expected state
        let weighted_pool = WeightedPool {
            pool_id,
            pool_address,
            tokens: tokens.clone(),
            specialization,
            block_created: 1,
        };
        let expected_pools = hashmap! { pool_id => weighted_pool };
        pools_by_token.insert(tokens[0], hashset! { pool_id });
        pools_by_token.insert(tokens[1], hashset! { pool_id });

        assert_eq!(pool_store.pools, expected_pools);
        assert_eq!(pool_store.pending_pools, HashMap::new());
        assert_eq!(pool_store.pools_by_token, pools_by_token);
    }

    #[test]
    fn balancer_delete_pools() {
        // Construct a bunch of pools
        let mut setup = dummy_balancer_setup(0, 2);

        setup.pool_store.delete_pools(1).unwrap();

        assert_eq!(setup.pool_store.last_event_block(), 0);
        assert!(setup.pool_store.pools.get(&setup.pool_ids[0]).is_some());
        assert!(!setup
            .pool_store
            .pools_by_token
            .get(&setup.tokens[0])
            .unwrap()
            .is_empty());
        assert!(!setup
            .pool_store
            .pools_by_token
            .get(&setup.tokens[1])
            .unwrap()
            .is_empty());

        for i in 1..3 {
            assert!(!setup.pool_store.pools.contains_key(&setup.pool_ids[i]));
            assert!(setup
                .pool_store
                .pools_by_token
                .get(&setup.tokens[i + 1])
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn balancer_insert_events() {
        let n = 3usize;
        let pool_ids: Vec<H256> = (0..n).map(|i| H256::from_low_u64_be(i as u64)).collect();
        let pool_addresses: Vec<H160> = (0..n).map(|i| H160::from_low_u64_be(i as u64)).collect();
        let tokens: Vec<H160> = (0..n + 1)
            .map(|i| H160::from_low_u64_be(i as u64))
            .collect();
        let specializations: Vec<PoolSpecialization> = (0..n)
            .map(|i| PoolSpecialization::new(i as u8 % 3).unwrap())
            .collect();
        let pool_registration_events: Vec<BalancerEvent> = (0..n)
            .map(|i| {
                BalancerEvent::PoolRegistered(PoolRegistered {
                    pool_id: pool_ids[i],
                    pool_address: pool_addresses[i],
                    specialization: specializations[i],
                })
            })
            .collect();
        let token_registration_events: Vec<BalancerEvent> = (0..n)
            .map(|i| {
                BalancerEvent::TokensRegistered(TokensRegistered {
                    pool_id: pool_ids[i],
                    tokens: vec![tokens[i], tokens[i + 1]],
                })
            })
            .collect();

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
        assert_eq!(
            pool_store.pools_by_token.get(&tokens[0]).unwrap(),
            &hashset! { pool_ids[0] }
        );
        assert_eq!(
            pool_store.pools_by_token.get(&tokens[1]).unwrap(),
            &hashset! { pool_ids[0], pool_ids[1] }
        );
        assert_eq!(
            pool_store.pools_by_token.get(&tokens[2]).unwrap(),
            &hashset! { pool_ids[1], pool_ids[2] }
        );
        assert_eq!(
            pool_store.pools_by_token.get(&tokens[3]).unwrap(),
            &hashset! { pool_ids[2] }
        );

        assert_eq!(
            pool_store.pools.get(&pool_ids[0]).unwrap(),
            &WeightedPool {
                pool_id: pool_ids[0],
                pool_address: pool_addresses[0],
                tokens: vec![tokens[0], tokens[1]],
                specialization: PoolSpecialization::new(0).unwrap(),
                block_created: 1
            }
        );
        assert_eq!(
            pool_store.pools.get(&pool_ids[1]).unwrap(),
            &WeightedPool {
                pool_id: pool_ids[1],
                pool_address: pool_addresses[1],
                tokens: vec![tokens[1], tokens[2]],
                specialization: PoolSpecialization::new(1).unwrap(),
                block_created: 1
            }
        );
        assert_eq!(
            pool_store.pools.get(&pool_ids[2]).unwrap(),
            &WeightedPool {
                pool_id: pool_ids[2],
                pool_address: pool_addresses[2],
                tokens: vec![tokens[2], tokens[3]],
                specialization: PoolSpecialization::new(2).unwrap(),
                block_created: 3
            }
        );
    }

    struct BalancerPoolSetup {
        pool_ids: Vec<H256>,
        pool_addresses: Vec<H160>,
        tokens: Vec<H160>,
        specializations: Vec<PoolSpecialization>,
        pool_store: BalancerPools,
    }

    fn dummy_balancer_setup(start_block: usize, end_block: usize) -> BalancerPoolSetup {
        let pool_ids: Vec<H256> = (start_block..end_block + 1)
            .map(|i| H256::from_low_u64_be(i as u64))
            .collect();
        let pool_addresses: Vec<H160> = (start_block..end_block + 1)
            .map(|i| H160::from_low_u64_be(i as u64))
            .collect();
        let tokens: Vec<H160> = (start_block..end_block + 2)
            .map(|i| H160::from_low_u64_be(i as u64))
            .collect();
        let specializations: Vec<PoolSpecialization> = (start_block..end_block + 1)
            .map(|i| PoolSpecialization::new(i as u8 % 3).unwrap())
            .collect();
        let pool_registration_events: Vec<BalancerEvent> = (start_block..end_block + 1)
            .map(|i| {
                BalancerEvent::PoolRegistered(PoolRegistered {
                    pool_id: pool_ids[i],
                    pool_address: pool_addresses[i],
                    specialization: specializations[i],
                })
            })
            .collect();
        let token_registration_events: Vec<BalancerEvent> = (start_block..end_block + 1)
            .map(|i| {
                BalancerEvent::TokensRegistered(TokensRegistered {
                    pool_id: pool_ids[i],
                    tokens: vec![tokens[i], tokens[i + 1]],
                })
            })
            .collect();

        let balancer_events: Vec<(EventIndex, BalancerEvent)> = (start_block..end_block + 1)
            .map(|i| {
                vec![
                    (
                        EventIndex::new(i as u64, 0),
                        pool_registration_events[i].clone(),
                    ),
                    (
                        EventIndex::new(i as u64, 1),
                        token_registration_events[i].clone(),
                    ),
                ]
            })
            .flatten()
            .collect();

        let mut pool_store = BalancerPools::default();
        pool_store.insert_events(balancer_events).unwrap();
        BalancerPoolSetup {
            pool_ids,
            pool_addresses,
            tokens,
            specializations,
            pool_store,
        }
    }

    #[test]
    fn balancer_replace_events() {
        let mut setup = dummy_balancer_setup(0, 5);
        assert_eq!(setup.pool_store.last_event_block(), 5);
        let new_pool_id_a = H256::from_low_u64_be(43110);
        let new_pool_id_b = H256::from_low_u64_be(1337);
        let new_pool_address = H160::zero();
        let new_token = H160::from_low_u64_be(808);
        let new_pool_registration = PoolRegistered {
            pool_id: new_pool_id_a,
            pool_address: new_pool_address,
            specialization: PoolSpecialization::General,
        };
        let new_token_registration = TokensRegistered {
            pool_id: new_pool_id_b,
            tokens: vec![new_token],
        };

        let new_events = vec![
            (
                EventIndex::new(3, 0),
                BalancerEvent::PoolRegistered(new_pool_registration.clone()),
            ),
            (
                EventIndex::new(4, 0),
                BalancerEvent::TokensRegistered(new_token_registration.clone()),
            ),
        ];
        setup
            .pool_store
            .replace_events(3, new_events.clone())
            .unwrap();
        // Everything until block 3 is unchanged.
        for i in 0..3 {
            assert_eq!(
                setup.pool_store.pools.get(&setup.pool_ids[i]).unwrap(),
                &WeightedPool {
                    pool_id: setup.pool_ids[i],
                    pool_address: setup.pool_addresses[i],
                    tokens: vec![setup.tokens[i], setup.tokens[i + 1]],
                    specialization: setup.specializations[i],
                    block_created: i as u64
                }
            );
        }
        assert_eq!(
            setup
                .pool_store
                .pools_by_token
                .get(&setup.tokens[0])
                .unwrap(),
            &hashset! { setup.pool_ids[0] }
        );
        assert_eq!(
            setup
                .pool_store
                .pools_by_token
                .get(&setup.tokens[1])
                .unwrap(),
            &hashset! { setup.pool_ids[0], setup.pool_ids[1] }
        );
        assert_eq!(
            setup
                .pool_store
                .pools_by_token
                .get(&setup.tokens[2])
                .unwrap(),
            &hashset! { setup.pool_ids[1], setup.pool_ids[2] }
        );
        assert_eq!(
            setup
                .pool_store
                .pools_by_token
                .get(&setup.tokens[3])
                .unwrap(),
            &hashset! { setup.pool_ids[2] }
        );

        // Everything old from block 3 on is gone.
        for i in 3..6 {
            assert!(setup.pool_store.pools.get(&setup.pool_ids[i]).is_none());
        }
        for i in 4..7 {
            assert!(setup
                .pool_store
                .pools_by_token
                .get(&setup.tokens[i])
                .unwrap()
                .is_empty());
        }

        // All new data is included.
        assert_eq!(
            setup.pool_store.pending_pools.get(&new_pool_id_a).unwrap(),
            &WeightedPoolBuilder {
                pool_registration: Some(new_pool_registration),
                tokens_registration: None,
                block_created: new_events[0].0.block_number
            }
        );
        assert_eq!(
            setup.pool_store.pending_pools.get(&new_pool_id_b).unwrap(),
            &WeightedPoolBuilder {
                pool_registration: None,
                tokens_registration: Some(new_token_registration),
                block_created: new_events[1].0.block_number
            }
        );

        assert!(setup.pool_store.pools_by_token.get(&new_token).is_none());
        assert_eq!(setup.pool_store.last_event_block(), 4);
    }
}
