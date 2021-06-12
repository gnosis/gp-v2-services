use crate::{
    current_block::BlockRetrieving,
    event_handling::{BlockNumber, EventHandler, EventIndex, EventStoring},
    impl_event_retrieving,
    maintenance::Maintaining,
    Web3,
};
use anyhow::{anyhow, Context, Result};
use contracts::{
    balancer_v2_weighted_pool_2_tokens_factory::{
        self, event_data::PoolCreated as TwoTokenPoolCreated,
        Event as WeightedPool2TokensFactoryEvent,
    },
    balancer_v2_weighted_pool_factory::{
        self, event_data::PoolCreated as WeightedPoolCreated, Event as WeightedPoolFactoryEvent,
    },
    BalancerV2Vault, BalancerV2WeightedPool, BalancerV2WeightedPool2TokensFactory,
    BalancerV2WeightedPoolFactory,
};
use derivative::Derivative;
use ethcontract::common::DeploymentInformation;
use ethcontract::{
    dyns::DynWeb3, Bytes, Event as EthContractEvent, EventMetadata, H160, H256, U256,
};
use itertools::Itertools;
use mockall::*;
use model::TokenPair;
use std::sync::Arc;
use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    ops::RangeInclusive,
};
use tokio::sync::Mutex;

#[derive(Copy, Debug, Default, Clone, Eq, PartialEq, Hash)]
pub struct PoolCreated {
    pub pool_address: H160,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum PoolType {
    WeightedGeneral,
    WeightedTwoToken,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct RegisteredWeightedPool {
    pub pool_id: H256,
    pub pool_address: H160,
    pub tokens: Vec<H160>,
    pub pool_type: PoolType,
    pub normalized_weights: Vec<U256>,
    pub(crate) block_created: u64,
}

impl RegisteredWeightedPool {
    /// Errors expected here are propagated from `get_pool_data`.
    async fn from_event(
        block_created: u64,
        creation: PoolCreated,
        pool_type: PoolType,
        data_fetcher: &dyn PoolDataFetching,
    ) -> Result<RegisteredWeightedPool> {
        let pool_address = creation.pool_address;
        let pool_data = data_fetcher.get_pool_data(pool_address).await?;
        return Ok(RegisteredWeightedPool {
            pool_id: pool_data.pool_id,
            pool_address,
            pool_type,
            tokens: pool_data.tokens,
            normalized_weights: pool_data.weights,
            block_created,
        });
    }
}

#[derive(Clone)]
pub struct WeightedPoolData {
    pool_id: H256,
    tokens: Vec<H160>,
    weights: Vec<U256>,
}

#[automock]
#[async_trait::async_trait]
trait PoolDataFetching: Send + Sync {
    async fn get_pool_data(&self, pool_address: H160) -> Result<WeightedPoolData>;
}

#[async_trait::async_trait]
impl PoolDataFetching for Web3 {
    /// Could result in ethcontract::{NodeError, MethodError or ContractError}
    async fn get_pool_data(&self, pool_address: H160) -> Result<WeightedPoolData> {
        let pool_contract = BalancerV2WeightedPool::at(self, pool_address);
        // Need vault and pool_id before we can fetch tokens.
        let vault = BalancerV2Vault::deployed(&self).await?;
        let pool_id = H256::from(pool_contract.methods().get_pool_id().call().await?.0);
        let tokens = vault
            .methods()
            .get_pool_tokens(Bytes(pool_id.0))
            .call()
            .await?
            .0;
        Ok(WeightedPoolData {
            pool_id,
            tokens,
            weights: pool_contract
                .methods()
                .get_normalized_weights()
                .call()
                .await?,
        })
    }
}

/// The BalancerPool struct represents in-memory storage of all deployed Balancer Pools
#[derive(Derivative)]
#[derivative(Debug)]
pub struct PoolRegistry {
    /// Used for O(1) access to all pool_ids for a given token
    pools_by_token: HashMap<H160, HashSet<H256>>,
    /// WeightedPool data for a given PoolId
    pools: HashMap<H256, RegisteredWeightedPool>,
    #[derivative(Debug = "ignore")]
    data_fetcher: Box<dyn PoolDataFetching>,
}

impl PoolRegistry {
    // Since all the fields are private, we expose helper methods to fetch relevant information
    /// Returns all pools containing both tokens from TokenPair
    pub fn pools_containing_token_pair(
        &self,
        token_pair: TokenPair,
    ) -> HashSet<RegisteredWeightedPool> {
        let empty_set = HashSet::new();
        let pools_0 = self
            .pools_by_token
            .get(&token_pair.get().0)
            .unwrap_or(&empty_set);
        let pools_1 = self
            .pools_by_token
            .get(&token_pair.get().1)
            .unwrap_or(&empty_set);
        pools_0
            .intersection(pools_1)
            .into_iter()
            .map(|pool_id| {
                self.pools
                    .get(pool_id)
                    .expect("failed iterating over known pools")
                    .clone()
            })
            .collect::<HashSet<RegisteredWeightedPool>>()
    }

    /// Given a collection of TokenPair, returns all pools containing at least one of the pairs.
    pub fn pools_containing_token_pairs(
        &self,
        token_pairs: HashSet<TokenPair>,
    ) -> HashSet<RegisteredWeightedPool> {
        token_pairs
            .into_iter()
            .flat_map(|pair| self.pools_containing_token_pair(pair))
            .unique_by(|pool| pool.pool_id)
            .collect()
    }

    async fn insert_events(
        &mut self,
        events: Vec<(EventIndex, PoolCreated)>,
        pool_type: PoolType,
    ) -> Result<()> {
        for (index, creation) in events {
            let weighted_pool = RegisteredWeightedPool::from_event(
                index.block_number,
                creation,
                pool_type,
                &*self.data_fetcher,
            )
            .await?;
            let pool_id = weighted_pool.pool_id;
            self.pools.insert(pool_id, weighted_pool.clone());
            for token in weighted_pool.tokens {
                self.pools_by_token
                    .entry(token)
                    .or_default()
                    .insert(pool_id);
            }
        }
        Ok(())
    }

    async fn replace_events_inner(
        &mut self,
        delete_from_block_number: u64,
        pool_type: PoolType,
        events: Vec<(EventIndex, PoolCreated)>,
    ) -> Result<()> {
        self.delete_pools(delete_from_block_number, pool_type)?;
        self.insert_events(events, pool_type).await?;
        Ok(())
    }

    fn delete_pools(&mut self, delete_from_block_number: u64, pool_type: PoolType) -> Result<()> {
        self.pools.retain(|_, pool| {
            pool.block_created < delete_from_block_number || pool.pool_type != pool_type
        });
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

    fn last_event_block(&self, pool_type: PoolType) -> u64 {
        // Technically we could keep this updated more effectively in a field on balancer pools,
        // but the maintenance seems like more overhead that needs to be tested.
        self.pools
            .iter()
            .filter_map(|(_, pool)| {
                if pool.pool_type == pool_type {
                    Some(pool.block_created)
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(0)
    }

    fn contract_to_balancer_events_weighted_pool(
        &self,
        contract_events: Vec<EthContractEvent<WeightedPoolFactoryEvent>>,
    ) -> Result<Vec<(EventIndex, PoolCreated)>> {
        contract_events
            .into_iter()
            .map(|EthContractEvent { data, meta }| {
                let meta = match meta {
                    Some(meta) => meta,
                    None => return Err(anyhow!("event without metadata")),
                };
                match data {
                    WeightedPoolFactoryEvent::PoolCreated(event) => {
                        convert_weighted_pool_created(&event, &meta)
                    }
                }
            })
            .collect::<Result<Vec<_>>>()
    }

    fn contract_to_balancer_events_two_token(
        &self,
        contract_events: Vec<EthContractEvent<WeightedPool2TokensFactoryEvent>>,
    ) -> Result<Vec<(EventIndex, PoolCreated)>> {
        contract_events
            .into_iter()
            .map(|EthContractEvent { data, meta }| {
                let meta = match meta {
                    Some(meta) => meta,
                    None => return Err(anyhow!("event without metadata")),
                };
                match data {
                    WeightedPool2TokensFactoryEvent::PoolCreated(event) => {
                        convert_two_token_pool_created(&event, &meta)
                    }
                }
            })
            .collect::<Result<Vec<_>>>()
    }
}

pub struct BalancerEventUpdater {
    weighted_pool_updater: Mutex<
        EventHandler<DynWeb3, BalancerV2WeightedPoolFactoryContract, Arc<Mutex<PoolRegistry>>>,
    >,
    two_token_pool_updater: Mutex<
        EventHandler<
            DynWeb3,
            BalancerV2WeightedPool2TokensFactoryContract,
            Arc<Mutex<PoolRegistry>>,
        >,
    >,
}

impl BalancerEventUpdater {
    pub async fn new(
        weighted_pool_factory: BalancerV2WeightedPoolFactory,
        two_token_pool_factory: BalancerV2WeightedPool2TokensFactory,
        pools: PoolRegistry,
    ) -> Result<Self> {
        // Choosing any one of the web3s to be used all over.
        let web3 = weighted_pool_factory.raw_instance().web3();
        let store = Arc::new(Mutex::new(pools));
        let deployment_block_weighted_pool =
            get_deployment_block(weighted_pool_factory.deployment_information(), &web3).await;
        let deployment_block_two_token_pool =
            get_deployment_block(two_token_pool_factory.deployment_information(), &web3).await;
        let weighted_pool_updater = Mutex::new(EventHandler::new(
            web3.clone(),
            BalancerV2WeightedPoolFactoryContract(weighted_pool_factory),
            store.clone(),
            deployment_block_weighted_pool,
        ));
        let two_token_pool_updater = Mutex::new(EventHandler::new(
            web3,
            BalancerV2WeightedPool2TokensFactoryContract(two_token_pool_factory),
            store,
            deployment_block_two_token_pool,
        ));
        Ok(Self {
            weighted_pool_updater,
            two_token_pool_updater,
        })
    }
}

async fn get_deployment_block(
    deployment_info: Option<DeploymentInformation>,
    web3: &DynWeb3,
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
impl EventStoring<WeightedPoolFactoryEvent> for Arc<Mutex<PoolRegistry>> {
    async fn replace_events(
        &mut self,
        events: Vec<EthContractEvent<WeightedPoolFactoryEvent>>,
        range: RangeInclusive<BlockNumber>,
    ) -> Result<()> {
        self.lock().await.replace_events(events, range).await
    }

    async fn append_events(
        &mut self,
        events: Vec<EthContractEvent<WeightedPoolFactoryEvent>>,
    ) -> Result<()> {
        self.lock().await.append_events(events).await
    }

    async fn last_event_block(&self) -> Result<u64> {
        Ok(self
            .lock()
            .await
            .last_event_block(PoolType::WeightedGeneral))
    }
}

#[async_trait::async_trait]
impl EventStoring<WeightedPool2TokensFactoryEvent> for Arc<Mutex<PoolRegistry>> {
    async fn replace_events(
        &mut self,
        events: Vec<EthContractEvent<WeightedPool2TokensFactoryEvent>>,
        range: RangeInclusive<BlockNumber>,
    ) -> Result<()> {
        self.lock().await.replace_events(events, range).await
    }

    async fn append_events(
        &mut self,
        events: Vec<EthContractEvent<WeightedPool2TokensFactoryEvent>>,
    ) -> Result<()> {
        self.lock().await.append_events(events).await
    }

    async fn last_event_block(&self) -> Result<u64> {
        Ok(self
            .lock()
            .await
            .last_event_block(PoolType::WeightedTwoToken))
    }
}

#[async_trait::async_trait]
impl EventStoring<WeightedPoolFactoryEvent> for PoolRegistry {
    async fn replace_events(
        &mut self,
        events: Vec<EthContractEvent<WeightedPoolFactoryEvent>>,
        range: RangeInclusive<BlockNumber>,
    ) -> Result<()> {
        let balancer_events = self
            .contract_to_balancer_events_weighted_pool(events)
            .context("failed to convert events")?;
        tracing::debug!(
            "replacing {} events from block number {}",
            balancer_events.len(),
            range.start().to_u64()
        );
        PoolRegistry::replace_events_inner(self, 0, PoolType::WeightedGeneral, balancer_events)
            .await?;
        Ok(())
    }

    async fn append_events(
        &mut self,
        events: Vec<EthContractEvent<WeightedPoolFactoryEvent>>,
    ) -> Result<()> {
        let balancer_events = self
            .contract_to_balancer_events_weighted_pool(events)
            .context("failed to convert events")?;
        self.insert_events(balancer_events, PoolType::WeightedGeneral)
            .await
    }

    async fn last_event_block(&self) -> Result<u64> {
        Ok(self.last_event_block(PoolType::WeightedGeneral))
    }
}

#[async_trait::async_trait]
impl EventStoring<WeightedPool2TokensFactoryEvent> for PoolRegistry {
    async fn replace_events(
        &mut self,
        events: Vec<EthContractEvent<WeightedPool2TokensFactoryEvent>>,
        range: RangeInclusive<BlockNumber>,
    ) -> Result<()> {
        let balancer_events = self
            .contract_to_balancer_events_two_token(events)
            .context("failed to convert events")?;
        tracing::debug!(
            "replacing {} events from block number {}",
            balancer_events.len(),
            range.start().to_u64()
        );
        PoolRegistry::replace_events_inner(self, 0, PoolType::WeightedTwoToken, balancer_events)
            .await?;
        Ok(())
    }

    async fn append_events(
        &mut self,
        events: Vec<EthContractEvent<WeightedPool2TokensFactoryEvent>>,
    ) -> Result<()> {
        let balancer_events = self
            .contract_to_balancer_events_two_token(events)
            .context("failed to convert events")?;
        self.insert_events(balancer_events, PoolType::WeightedTwoToken)
            .await
    }

    async fn last_event_block(&self) -> Result<u64> {
        Ok(self.last_event_block(PoolType::WeightedTwoToken))
    }
}

impl_event_retrieving! {
    pub BalancerV2WeightedPoolFactoryContract for balancer_v2_weighted_pool_factory
}

impl_event_retrieving! {
    pub BalancerV2WeightedPool2TokensFactoryContract for balancer_v2_weighted_pool_2_tokens_factory
}

#[async_trait::async_trait]
impl Maintaining for BalancerEventUpdater {
    async fn run_maintenance(&self) -> Result<()> {
        self.two_token_pool_updater.run_maintenance().await?;
        self.weighted_pool_updater.run_maintenance().await
    }
}

fn convert_weighted_pool_created(
    creation: &WeightedPoolCreated,
    meta: &EventMetadata,
) -> Result<(EventIndex, PoolCreated)> {
    Ok((
        EventIndex::from(meta),
        PoolCreated {
            pool_address: creation.pool,
        },
    ))
}

fn convert_two_token_pool_created(
    creation: &TwoTokenPoolCreated,
    meta: &EventMetadata,
) -> Result<(EventIndex, PoolCreated)> {
    Ok((
        EventIndex::from(meta),
        PoolCreated {
            pool_address: creation.pool,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use maplit::hashset;
    use mockall::predicate::eq;

    #[tokio::test]
    async fn balancer_insert_events() {
        let pool_type = PoolType::WeightedGeneral;
        let n = 3usize;
        let pool_ids: Vec<H256> = (0..n).map(|i| H256::from_low_u64_be(i as u64)).collect();
        let pool_addresses: Vec<H160> = (0..n).map(|i| H160::from_low_u64_be(i as u64)).collect();
        let tokens: Vec<H160> = (0..n + 1)
            .map(|i| H160::from_low_u64_be(i as u64))
            .collect();
        let weights: Vec<U256> = (0..n + 1).map(|i| U256::from(i as u64)).collect();
        let creation_events: Vec<PoolCreated> = (0..n)
            .map(|i| PoolCreated {
                pool_address: pool_addresses[i],
            })
            .collect();

        let events: Vec<(EventIndex, PoolCreated)> = vec![
            (EventIndex::new(1, 0), creation_events[0]),
            (EventIndex::new(2, 0), creation_events[1]),
            (EventIndex::new(3, 0), creation_events[2]),
        ];

        let mut dummy_data_fetcher = MockPoolDataFetching::new();

        for i in 0..n {
            let expected_pool_data = WeightedPoolData {
                pool_id: pool_ids[i],
                tokens: vec![tokens[i], tokens[i + 1]],
                weights: vec![weights[i], weights[i + 1]],
            };
            dummy_data_fetcher
                .expect_get_pool_data()
                .with(eq(pool_addresses[i]))
                .returning(move |_| Ok(expected_pool_data.clone()));
        }

        let mut pool_store = PoolRegistry {
            pools_by_token: Default::default(),
            pools: Default::default(),
            data_fetcher: Box::new(dummy_data_fetcher),
        };
        pool_store.insert_events(events, pool_type).await.unwrap();
        // Note that it is never expected that blocks for events will differ,
        // but in this test block_created for the pool is the first block it receives.
        assert_eq!(pool_store.last_event_block(pool_type), 3);
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
        for i in 0..n {
            assert_eq!(
                pool_store.pools.get(&pool_ids[i]).unwrap(),
                &RegisteredWeightedPool {
                    pool_id: pool_ids[i],
                    pool_address: pool_addresses[i],
                    tokens: vec![tokens[i], tokens[i + 1]],
                    pool_type,
                    normalized_weights: vec![weights[i], weights[i + 1]],
                    block_created: i as u64 + 1
                },
                "failed assertion at index {}",
                i
            );
        }
    }

    #[tokio::test]
    async fn balancer_replace_events() {
        let pool_type = PoolType::WeightedGeneral;
        let start_block = 0;
        let end_block = 5;
        // Setup all the variables to initialize Balancer Pool State
        let pool_ids: Vec<H256> = (start_block..end_block + 1)
            .map(|i| H256::from_low_u64_be(i as u64))
            .collect();
        let pool_addresses: Vec<H160> = (start_block..end_block + 1)
            .map(|i| H160::from_low_u64_be(i as u64))
            .collect();
        let tokens: Vec<H160> = (start_block..end_block + 2)
            .map(|i| H160::from_low_u64_be(i as u64))
            .collect();
        let weights: Vec<U256> = (start_block..end_block + 2)
            .map(|i| U256::from(i as u64))
            .collect();
        let creation_events: Vec<PoolCreated> = (start_block..end_block + 1)
            .map(|i| PoolCreated {
                pool_address: pool_addresses[i],
            })
            .collect();

        let converted_events: Vec<(EventIndex, PoolCreated)> = (start_block..end_block + 1)
            .map(|i| (EventIndex::new(i as u64, 0), creation_events[i]))
            .collect();

        let mut dummy_data_fetcher = MockPoolDataFetching::new();
        for i in start_block..end_block + 1 {
            let expected_pool_data = WeightedPoolData {
                pool_id: pool_ids[i],
                tokens: vec![tokens[i], tokens[i + 1]],
                weights: vec![weights[i], weights[i + 1]],
            };
            dummy_data_fetcher
                .expect_get_pool_data()
                .with(eq(pool_addresses[i]))
                .returning(move |_| Ok(expected_pool_data.clone()));
        }

        // Have to prepare return data for new stuff before we pass on the data fetcher
        let new_pool_id = H256::from_low_u64_be(43110);
        let new_pool_address = H160::from_low_u64_be(42);
        let new_token = H160::from_low_u64_be(808);
        let new_weight = U256::from(1337);
        let new_creation = PoolCreated {
            pool_address: new_pool_address,
        };
        let new_event = (EventIndex::new(3, 0), new_creation);
        dummy_data_fetcher
            .expect_get_pool_data()
            .with(eq(new_pool_address))
            .returning(move |_| {
                Ok(WeightedPoolData {
                    pool_id: new_pool_id,
                    tokens: vec![new_token],
                    weights: vec![new_weight],
                })
            });

        let mut pool_store = PoolRegistry {
            pools_by_token: Default::default(),
            pools: Default::default(),
            data_fetcher: Box::new(dummy_data_fetcher),
        };
        pool_store
            .insert_events(converted_events, pool_type)
            .await
            .unwrap();
        // Let the tests begin!
        assert_eq!(pool_store.last_event_block(pool_type), end_block as u64);
        pool_store
            .replace_events_inner(3, pool_type, vec![new_event])
            .await
            .unwrap();

        // Everything until block 3 is unchanged.
        for i in 0..3 {
            assert_eq!(
                pool_store.pools.get(&pool_ids[i]).unwrap(),
                &RegisteredWeightedPool {
                    pool_id: pool_ids[i],
                    pool_address: pool_addresses[i],
                    tokens: vec![tokens[i], tokens[i + 1]],
                    pool_type,
                    normalized_weights: vec![weights[i], weights[i + 1]],
                    block_created: i as u64
                },
                "assertion failed at index {}",
                i
            );
        }
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

        // Everything old from block 3 on is gone.
        for pool_id in pool_ids.iter().take(6).skip(3) {
            assert!(pool_store.pools.get(pool_id).is_none());
        }
        for token in tokens.iter().take(7).skip(4) {
            assert!(pool_store.pools_by_token.get(token).unwrap().is_empty());
        }

        let new_event_block = new_event.0.block_number;

        // All new data is included.
        assert_eq!(
            pool_store.pools.get(&new_pool_id).unwrap(),
            &RegisteredWeightedPool {
                pool_id: new_pool_id,
                pool_address: new_pool_address,
                tokens: vec![new_token],
                pool_type,
                normalized_weights: vec![new_weight],
                block_created: new_event_block
            }
        );

        assert!(pool_store.pools_by_token.get(&new_token).is_some());
        assert_eq!(pool_store.last_event_block(pool_type), new_event_block);
    }

    #[test]
    fn pools_containing_pair_test() {
        let n = 3;
        let pool_ids: Vec<H256> = (0..n).map(|i| H256::from_low_u64_be(i as u64)).collect();
        let pool_addresses: Vec<H160> = (0..n)
            .map(|i| H160::from_low_u64_be(2 * i as u64))
            .collect();
        let tokens: Vec<H160> = (0..n)
            .map(|i| H160::from_low_u64_be(2 * i as u64 + 1))
            .collect();
        let token_pairs: Vec<TokenPair> = (0..n)
            .map(|i| TokenPair::new(tokens[i], tokens[(i + 1) % n]).unwrap())
            .collect();

        let mut dummy_data_fetcher = MockPoolDataFetching::new();
        // Have to load all expected data into fetcher before it is passed on.
        for i in 0..n {
            let expected_pool_data = WeightedPoolData {
                pool_id: pool_ids[i],
                tokens: tokens[i..n].to_owned(),
                weights: vec![],
            };
            dummy_data_fetcher
                .expect_get_pool_data()
                .with(eq(pool_addresses[i]))
                .returning(move |_| Ok(expected_pool_data.clone()));
        }
        // Test the empty pool.
        let mut pool_store = PoolRegistry {
            pools_by_token: Default::default(),
            pools: Default::default(),
            data_fetcher: Box::new(dummy_data_fetcher),
        };
        for token_pair in token_pairs.iter().take(n) {
            assert!(pool_store
                .pools_containing_token_pair(*token_pair)
                .is_empty());
        }

        // Now test non-empty pool with standard form.
        let mut weighted_pools = vec![];
        for i in 0..n {
            for pool_id in pool_ids.iter().take(i + 1) {
                // This is tokens[i] => { pool_id[0], pool_id[1], ..., pool_id[i] }
                let entry = pool_store.pools_by_token.entry(tokens[i]).or_default();
                entry.insert(*pool_id);
            }
            // This is weighted_pools[i] has tokens [tokens[i], tokens[i+1], ... , tokens[n]]
            weighted_pools.push(RegisteredWeightedPool {
                pool_id: pool_ids[i],
                tokens: tokens[i..n].to_owned(),
                normalized_weights: vec![],
                pool_type: PoolType::WeightedGeneral,
                block_created: 0,
                pool_address: pool_addresses[i],
            });
            pool_store
                .pools
                .insert(pool_ids[i], weighted_pools[i].clone());
        }
        // When n = 3, this above generates
        // pool_store.pools_by_token = hashmap! {
        //     tokens[0] => hashset! { pool_ids[0] },
        //     tokens[1] => hashset! { pool_ids[0], pool_ids[1]},
        //     tokens[2] => hashset! { pool_ids[0], pool_ids[1], pool_ids[2] },
        // };
        // pool_store.pools = hashmap! {
        //     pool_ids[0] => WeightedPool {
        //         tokens: vec![tokens[0], tokens[1], tokens[2]],
        //         ..other fields
        //     },
        //     pool_ids[1] => WeightedPool {
        //         tokens: vec![tokens[1], tokens[2]],
        //         ..other fields
        //     }
        //     pool_ids[2] => WeightedPool {
        //         tokens: vec![tokens[2]],
        //         ..other fields
        //     }
        // };

        assert_eq!(
            pool_store.pools_containing_token_pair(token_pairs[0]),
            hashset! { weighted_pools[0].clone() }
        );
        assert_eq!(
            pool_store.pools_containing_token_pair(token_pairs[1]),
            hashset! { weighted_pools[1].clone(), weighted_pools[0].clone() }
        );
        assert_eq!(
            pool_store.pools_containing_token_pair(token_pairs[2]),
            hashset! { weighted_pools[0].clone() }
        );
    }
}
