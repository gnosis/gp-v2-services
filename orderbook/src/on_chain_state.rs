use anyhow::{ensure, Result};
use contracts::GPv2Settlement;

use ethcontract::futures::stream::{Stream, StreamExt as _};
use ethcontract::{errors::EventError, BlockNumber, EventStatus};
use model::OrderUid;
use primitive_types::U256;
use std::collections::HashMap;
use tracing::info;
use web3::Web3;

type Event = ethcontract::Event<EventStatus<contracts::g_pv_2_settlement::event_data::Trade>>;

pub struct SettledOrderUpdater {
    settlement_contract: GPv2Settlement,
    web3: Web3<web3::transports::Http>,
    settled_orders: HashMap<OrderUid, U256>,
    last_handled_block: u64,
    block_page_size: u64,
}

// TODO: Implement proper revertion with BLOCK_CONFIRMATION_COUNT>0
const BLOCK_CONFIRMATION_COUNT: u64 = 0;

impl SettledOrderUpdater {
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
    async fn update_settled_orders(&mut self) -> Result<()> {
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
        info!(
            "Updating event based orderbook from block {} to block {}.",
            from_block, current_block,
        );
        self.update_with_events_between_blocks(from_block, current_block)
            .await
    }

    async fn update_with_events_between_blocks(
        &mut self,
        from_block: u64,
        to_block: u64,
    ) -> Result<()> {
        let mut events = self.chunked_events(from_block, to_block).await?;
        while let Some(chunk) = events.next().await {
            let events = chunk;
            for event in events {
                println!("{:?}", event);
                //ToDo remove the orders for now, as rinkeby does not reorg.
            }
            self.last_handled_block = to_block;
        }

        Ok(())
    }

    async fn chunked_events(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> Result<impl Stream<Item = Result<Vec<Event>, EventError>> + '_> {
        let trade_builder = self
            .settlement_contract
            .events()
            .trade()
            .from_block(BlockNumber::Number(from_block.into()))
            .to_block(BlockNumber::Number(to_block.into()));
        let event_stream = trade_builder.stream();
        let event_chunks = event_stream
            .ready_chunks(self.block_page_size as usize)
            .map(|chunk| chunk.into_iter().collect::<Result<Vec<_>, _>>());
        Ok(event_chunks)
    }
}
