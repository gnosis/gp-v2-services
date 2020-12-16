use anyhow::{ensure, Result};
use contracts::GPv2Settlement;
use ethcontract::log::LogFilterBuilder;

use ethcontract::{errors::ExecutionError, BlockNumber, H256};
use futures::stream::{Stream, StreamExt as _};
use model::{DomainSeparator, Order, OrderCreation, OrderMetaData, OrderUid};
use primitive_types::U256;
use std::collections::{hash_map::Entry, HashMap};
use web3::Web3;

type Event = ethcontract::contract::Event<contracts::GPv2Settlement::Event>;

pub struct OnChainStateStore {
    settlement_contract: GPv2Settlement,
    web3: Web3<web3::transports::Http>,
    settled_orders: HashMap<OrderUid, U256>, // maybe this needs to become a mutex;
    last_handled_block: u64,
    block_page_size: u64,
    settlement_event_filter: LogFilterBuilder<web3::transports::Http>,
}
const BLOCK_CONFIRMATION_COUNT: u64 = 2;

impl OnChainStateStore {
    fn new(contract: GPv2Settlement, web3: Web3<web3::transports::Http>) -> Self {
        Self {
            settlement_contract: contract,
            web3,
            settled_orders: HashMap::new(),
            last_handled_block: 0,
            block_page_size: 20,
        }
    }
    /// Gather all new events since the last update and update the orderbook.
    async fn update_settled_orders(&self) -> Result<()> {
        // We cannot use BlockNumber::Pending here because we are not guaranteed to get metadata for
        // pending blocks but we need the metadata in the functions below.
        let current_block = self.web3.eth().block_number().await?.as_u64();
        let from_block = self
            .last_handled_block
            .saturating_sub(BLOCK_CONFIRMATION_COUNT);
        ensure!(
            from_block <= current_block,
            format!(
                "current block number according to node is {} which is more than {} blocks in the \
             past compared to previous current block {}",
                current_block, BLOCK_CONFIRMATION_COUNT, from_block
            )
        );
        // log::info!(
        //     "Updating event based orderbook from block {} to block {}.",
        //     from_block,
        //     current_block,
        // );
        self.update_with_events_between_blocks(from_block, current_block)
            .await
    }

    async fn update_with_events_between_blocks(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> Result<()> {
        let mut events = self.chunked_events(from_block, to_block).await?;
        while let Some(chunk) = events.next().await {
            let events = chunk?;
            for event in events {
                println!("{:}", event);
            }
            self.last_handled_block = to_block;
        }

        Ok(())
    }

    async fn chunked_events(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> Result<impl Stream<Item = Result<Vec<Event>, ExecutionError>> + '_> {
        let event_stream = self
            .settlement_contract
            .all_events(
                BlockNumber::Number(from_block.into()),
                BlockNumber::Number(to_block.into()),
                self.block_page_size as _,
            )
            .await?;
        let event_chunks = event_stream
            .ready_chunks(self.block_page_size)
            .map(|chunk| chunk.into_iter().collect::<Result<Vec<_>, _>>());
        Ok(event_chunks)
    }
}
