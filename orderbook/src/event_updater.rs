use crate::database::{
    Database, Event as DbEvent, EventIndex as DbEventIndex, Invalidation as DbInvalidation,
    Settlement as DbSettlement, Trade as DbTrade,
};
use anyhow::{anyhow, Context, Result};
use contracts::{
    g_pv_2_settlement::{
        event_data::{
            OrderInvalidated as ContractInvalidation, Settlement as ContractSettlement,
            Trade as ContractTrade,
        },
        Event as ContractEvent,
    },
    GPv2Settlement,
};
use ethcontract::contract::AllEventsBuilder;
use ethcontract::{dyns::DynTransport, Event as EthcontractEvent, EventMetadata};
use model::order::OrderUid;
use shared::event_handling::{BlockNumber, EventHandler, EventRetrieving, EventStoring};
use std::{
    convert::TryInto,
    ops::{Deref, DerefMut, RangeInclusive},
};
use web3::Web3;

pub struct EventUpdater(EventHandler<GPv2SettlementContract, Database>);

impl Deref for EventUpdater {
    type Target = EventHandler<GPv2SettlementContract, Database>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for EventUpdater {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[async_trait::async_trait]
impl EventStoring<ContractEvent> for Database {
    async fn save_events(
        &self,
        events: Vec<EthcontractEvent<ContractEvent>>,
        range: Option<RangeInclusive<BlockNumber>>,
    ) -> Result<()> {
        let db_events = events
            .into_iter()
            .filter_map(|EthcontractEvent { data, meta }| {
                let meta = match meta {
                    Some(meta) => meta,
                    None => return Some(Err(anyhow!("event without metadata"))),
                };
                match data {
                    ContractEvent::Trade(event) => Some(convert_trade(&event, &meta)),
                    ContractEvent::Settlement(event) => Some(Ok(convert_settlement(&event, &meta))),
                    ContractEvent::OrderInvalidated(event) => {
                        Some(convert_invalidation(&event, &meta))
                    }
                    // TODO: handle new events
                    ContractEvent::Interaction(_) => None,
                    ContractEvent::PreSignature(_) => None,
                }
            })
            .collect::<Result<Vec<_>>>()
            .context("failed to get event")?;
        // This is the event saving for Trades.
        tracing::debug!("inserting {} new events", db_events.len());
        if let Some(range) = range {
            self.replace_events(range.start().to_u64(), db_events)
                .await
                .context("failed to replace trades")?;
        } else {
            self.insert_events(db_events)
                .await
                .context("failed to insert trades")?;
        }
        Ok(())
    }

    async fn last_event_block(&self) -> Result<u64> {
        self.block_number_of_most_recent_event().await
    }
}

pub struct GPv2SettlementContract(GPv2Settlement);

impl EventRetrieving for GPv2SettlementContract {
    type Event = ContractEvent;
    fn get_events(&self) -> AllEventsBuilder<DynTransport, Self::Event> {
        self.0.all_events()
    }

    fn web3(&self) -> Web3<DynTransport> {
        self.0.raw_instance().web3()
    }
}

impl EventUpdater {
    pub fn new(contract: GPv2Settlement, db: Database, start_sync_at_block: Option<u64>) -> Self {
        Self(EventHandler::new(
            GPv2SettlementContract(contract),
            db,
            start_sync_at_block,
        ))
    }
}

fn convert_trade(trade: &ContractTrade, meta: &EventMetadata) -> Result<(DbEventIndex, DbEvent)> {
    let order_uid = OrderUid(
        trade
            .order_uid
            .as_slice()
            .try_into()
            .context("trade event order_uid has wrong number of bytes")?,
    );
    let event = DbTrade {
        order_uid,
        sell_amount_including_fee: trade.sell_amount,
        buy_amount: trade.buy_amount,
        fee_amount: trade.fee_amount,
    };
    Ok((event_meta_to_index(meta), DbEvent::Trade(event)))
}

fn convert_settlement(
    settlement: &ContractSettlement,
    meta: &EventMetadata,
) -> (DbEventIndex, DbEvent) {
    let event = DbSettlement {
        solver: settlement.solver,
        transaction_hash: meta.transaction_hash,
    };
    (event_meta_to_index(meta), DbEvent::Settlement(event))
}

fn convert_invalidation(
    invalidation: &ContractInvalidation,
    meta: &EventMetadata,
) -> Result<(DbEventIndex, DbEvent)> {
    let order_uid = OrderUid(
        invalidation
            .order_uid
            .as_slice()
            .try_into()
            .context("invalidation event order_uid has wrong number of bytes")?,
    );
    let event = DbInvalidation { order_uid };
    Ok((event_meta_to_index(meta), DbEvent::Invalidation(event)))
}

// Converts EventMetaData to DbEventIndex struct
fn event_meta_to_index(meta: &EventMetadata) -> DbEventIndex {
    DbEventIndex {
        block_number: meta.block_number,
        log_index: meta.log_index as u64,
    }
}
