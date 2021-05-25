use crate::{
    current_block::BlockRetrieving,
    event_handling::{BlockNumber, EventHandler, EventIndex, EventStoring},
    impl_event_retrieving,
    maintenance::Maintaining,
};
use anyhow::{anyhow, Context, Result};
use contracts::{
    balancer_v2_vault::{
        self, event_data::PoolRegistered as ContractPoolRegistered, Event as ContractEvent,
    },
    BalancerV2Vault,
};
use ethcontract::common::DeploymentInformation;
use ethcontract::{dyns::DynWeb3, Event as EthContractEvent, EventMetadata, H160};
use std::ops::RangeInclusive;
use tokio::sync::Mutex;

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
    // pool_address: H160,
// tokens: Vec<H160>,
// pool_id:
// TODO - other stuff
}

#[derive(Default)]
pub struct BalancerPools {
    // pools: HashMap<H160, HashSet<WeightedPool>>,
// Block number of last update
// last_updated: u64,
}

impl BalancerPools {
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
                        Some(convert_pool_registered(&event, &meta))
                    }
                    _ => {
                        tracing::info!("Got {:?}", data);
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
    pub async fn new(contract: BalancerV2Vault, pools: BalancerPools) -> Self {
        let mut deployment_block = None;
        if let Some(deployment_info) = contract.deployment_information() {
            match deployment_info {
                DeploymentInformation::BlockNumber(block_number) => {
                    deployment_block = Some(block_number);
                }
                DeploymentInformation::TransactionHash(hash) => {
                    deployment_block = contract
                        .raw_instance()
                        .web3()
                        .block_number_from_tx_hash(hash)
                        .await;
                }
            }
        };
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
        &self,
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
        todo!()
    }

    async fn append_events(&self, events: Vec<EthContractEvent<ContractEvent>>) -> Result<()> {
        let balancer_events = self
            .contract_to_balancer_events(events)
            .context("failed to convert events")?;
        tracing::debug!("inserting {} new events", balancer_events.len());
        todo!()
    }

    async fn last_event_block(&self) -> Result<u64> {
        todo!()
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
        pool_id: registration.pool_id,
        pool_address: registration.pool_address,
        specialization: registration.specialization,
    };
    Ok((EventIndex::from(meta), BalancerEvent::PoolRegistered(event)))
}
