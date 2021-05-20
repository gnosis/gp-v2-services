use crate::{
    event_handling::{BlockNumber, EventHandler, EventStoring},
    impl_event_retrieving,
    maintenance::Maintaining,
};
use anyhow::{anyhow, Context, Result};
use contracts::{
    balancer_v2::{self, Event as ContractEvent, event_data::PoolRegistered as ContractPoolRegistered},
    Vault,
};
use ethcontract::{dyns::DynWeb3, Event as EthContractEvent, H160, EventMetadata};
use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use tokio::sync::Mutex;
use crate::event_handling::EventIndex;

#[derive(Debug)]
pub enum BalancerEvent {
    PoolRegistered(PoolRegistered),
}

#[derive(Debug, Default)]
pub struct PoolRegistered {
    pub pool_id: [u8; 32],
    pub pool_address: H160,
    pub specialization: u8,
}


#[derive(Default)]
pub struct WeightedPool {
    pool_address: H160,
    tokens: Vec<H160>,
    // pool_id:
    // TODO - other stuff
}

#[derive(Default)]
pub struct BalancerPools {
    pools: HashMap<H160, HashSet<WeightedPool>>,
    // Block number of last update
    last_updated: u64,
}

impl BalancerPools {
    fn update_last_block(mut self, value: u64) {
        self.last_updated = value;
    }

    pub fn contract_to_balancer_events(
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
                    ContractEvent::PoolRegistered(event) => Some(convert_pool_registered(&event, &meta)),
                    _ => {
                        tracing::info!("Got {:?}", data);
                        None
                    }
                }
            })
            .collect::<Result<Vec<_>>>()
    }
}

pub struct BalancerEventUpdater(Mutex<EventHandler<DynWeb3, VaultContract, BalancerPools>>);

impl BalancerEventUpdater {
    pub fn new(contract: Vault, pools: BalancerPools, start_sync_at_block: Option<u64>) -> Self {
        Self(Mutex::new(EventHandler::new(
            contract.raw_instance().web3(),
            VaultContract(contract),
            pools,
            start_sync_at_block,
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
        let balancer_events = self.contract_to_balancer_events(events).context("failed to convert events")?;
        tracing::debug!(
            "replacing {} events from block number {}",
            balancer_events.len(),
            range.start().to_u64()
        );
        // TODO - implement event replace.
        Ok(())
    }

    async fn append_events(&mut self, events: Vec<EthContractEvent<ContractEvent>>) -> Result<()> {
        let balancer_events = self.contract_to_balancer_events(events).context("failed to convert events")?;
        tracing::debug!("inserting {} new events", balancer_events.len());
        // TODO - implement event append
        Ok(())
    }

    async fn last_event_block(&self) -> Result<u64> {
        Ok(self.last_updated)
    }
}

impl_event_retrieving! {
    pub VaultContract for balancer_v2
}

#[async_trait::async_trait]
impl Maintaining for BalancerEventUpdater {
    async fn run_maintenance(&self) -> Result<()> {
        self.0.run_maintenance().await
    }
}

fn convert_pool_registered(registration: &ContractPoolRegistered, meta: &EventMetadata) -> Result<(EventIndex, BalancerEvent)> {
    let event = PoolRegistered {
        pool_id: registration.pool_id,
        pool_address: registration.pool_address,
        specialization: registration.specialization
    };
    Ok((EventIndex::from(meta), BalancerEvent::PoolRegistered(event)))
}