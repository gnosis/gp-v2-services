use anyhow::bail;
use chrono::{offset::Utc, DateTime, TimeZone};
use contracts::GPv2Settlement;
use futures::{pin_mut, stream, Stream, StreamExt};
use shared::{transport::LoggingTransport, Web3};
use std::time::Duration;
use structopt::StructOpt;
use web3::{
    api::{EthFilter, Namespace},
    types::{Block, BlockId, BlockNumber, Transaction, TransactionReceipt, H160},
};

const MIN_TRANSACTIONS: usize = 1;
const MINUTES_BETWEEN_ALERTS: i64 = 10;
const BLOCK_POLL_DURATION: Duration = Duration::from_secs(20);
const NODE_RESPITE: Duration = Duration::from_secs(5);

fn parse_unit(text: &str) -> anyhow::Result<f64> {
    let num: f64 = text.parse()?;
    if !(0.0..=1.0).contains(&num) {
        bail!("not a number between zero and one");
    }
    Ok(num)
}

#[derive(Debug, StructOpt)]
struct Arguments {
    #[structopt(flatten)]
    shared: shared::arguments::Arguments,

    /// Which proportion of the previous transactions must fail before
    /// triggering an alert in a specified time window.
    /// Example: if the value is 0.1, then an alert is sent if more than 10% of
    /// the previous transactions fail.
    #[structopt(
        long,
        env = "REVERTED_TRANSACTIONS_ALERT_THRESHOLD",
        default_value = "0.3",
        parse(try_from_str = parse_unit),
    )]
    reverted_transaction_alert_threshold: f64,

    /// How much back in the past to check for failed transactions in seconds.
    #[structopt(
        long,
        env = "REVERTED_TRANSACTIONS_TIME_WINDOW",
        default_value = "600",
        parse(try_from_str = shared::arguments::duration_from_seconds),
    )]
    reverted_transaction_time_window: Duration,
}

async fn extract_transactions_to(
    block: Block<Transaction>,
    target: H160,
    web3: &Web3,
) -> impl Stream<Item = Option<TransactionReceipt>> + '_ {
    let transactions = block
        .transactions
        .into_iter()
        .filter(move |tx| tx.to == Some(target));
    stream::unfold(transactions, move |mut txs| {
        let eth = web3.eth();
        async move {
            let receipt = eth
                .transaction_receipt(txs.next()?.hash)
                .await
                .unwrap_or_else(|err| {
                    tracing::warn!("error connecting to node: {:?}", err);
                    None
                });
            Some((receipt, txs))
        }
    })
}

// Note: this function does not manage uncle blocks. It could be that more than
// one block is returned with the same number. Initialization may be subject to
// race conditions where a few blocks might be skipped or duplicated.
async fn get_blocks_from(
    start: DateTime<Utc>,
    web3: &Web3,
) -> impl Stream<Item = anyhow::Result<Block<Transaction>>> + '_ {
    let eth = web3.eth();
    let mut block_number = BlockNumber::Latest;
    let mut blocks = Vec::new();
    let filter = EthFilter::new(web3.transport())
        .create_blocks_filter()
        .await
        .expect("unable to register event listener for new blocks");
    let new_blocks = filter.stream(BLOCK_POLL_DURATION);
    loop {
        let block = eth.block_with_txs(BlockId::Number(block_number)).await;
        if let Err(e) = block {
            tracing::warn!("node error when fetching the block: {:?}", e);
            tokio::time::delay_for(NODE_RESPITE).await;
            continue;
        };
        let block = block.unwrap();
        if block.is_none() {
            tracing::warn!("no block found for block number {:?}", block_number);
            tokio::time::delay_for(NODE_RESPITE).await;
            continue;
        };
        let block = block.unwrap();

        tracing::info!(
            "retrieving past block {:?} at timestamp {:?}, hash {:?}",
            block.number,
            block.timestamp,
            block.hash
        );

        if Utc.timestamp(block.timestamp.as_u64() as i64, 0) < start {
            tracing::info!(
                "finished retrieving past blocks, first block outside of time range is block {:?}",
                block_number,
            );
            break;
        }

        block_number = BlockNumber::from(
            block
                .number
                .expect("block fetched by block number should have a number")
                - 1,
        );
        blocks.push(Ok(block));
    }

    stream::iter(blocks.into_iter().rev()).chain(new_blocks.then(move |block_hash| {
        let eth = web3.eth();
        async move {
            if let Err(e) = block_hash {
                // Note: this can happen if the node unregisters the block
                // filter. In this case, the new block filter might have to be
                // regenerated. This is not a panic to see
                tracing::error!("block filter did not return a valid block: {}", e);
                bail!("invalid block filter");
            }
            match eth.block_with_txs(BlockId::Hash(block_hash?)).await? {
                Some(block) => Ok(block),
                None => {
                    tracing::warn!("failed to retrieve a block returned by the new block filter");
                    bail!("invalid block from block filter")
                }
            }
        }
    }))
}

struct Settlement {
    time: DateTime<Utc>,
    success: bool,
}

struct Count {
    total: usize,
    failures: usize,
}

struct SettlementFailureCounter {
    // Note: settlements are not expected to be sorted by time because of uncle
    // blocks.
    settlements: Vec<Settlement>,
    time_window: chrono::Duration,
}
impl SettlementFailureCounter {
    fn new(time_window: Duration) -> Self {
        SettlementFailureCounter {
            settlements: Vec::new(),
            time_window: chrono::Duration::from_std(time_window)
                .expect("value too large for the chrono library"),
        }
    }

    fn prune(&mut self) {
        let threshold = Utc::now() - self.time_window;
        self.settlements
            .retain(|Settlement { time, .. }| time >= &threshold);
    }

    fn push_settlement(&mut self, time: DateTime<Utc>, success: bool) {
        self.settlements.push(Settlement { time, success })
    }

    fn settlements_from<'a>(
        &'a self,
        start: &'a DateTime<Utc>,
    ) -> impl Iterator<Item = &'a Settlement> + 'a {
        self.settlements
            .iter()
            .filter(move |Settlement { time, .. }| start <= time)
    }

    fn latest_count(&self) -> Count {
        let mut total = 0;
        let mut failures = 0;
        for settlement in self.settlements_from(&(Utc::now() - self.time_window)) {
            total += 1;
            if !settlement.success {
                failures += 1;
            }
        }
        Count { total, failures }
    }
}

#[tokio::main]
async fn main() {
    let args = Arguments::from_args();
    shared::tracing::initialize(args.shared.log_filter.as_str());
    tracing::info!("running monitoring service with {:#?}", args);

    let transport = LoggingTransport::new(
        web3::transports::Http::new(args.shared.node_url.as_str())
            .expect("transport creation failed"),
    );
    let web3 = web3::Web3::new(transport);
    let settlement_contract = GPv2Settlement::deployed(&web3)
        .await
        .expect("couldn't load deployed settlement");

    let mut counter = SettlementFailureCounter::new(args.reverted_transaction_time_window);

    let start =
        Utc::now() - chrono::Duration::from_std(args.reverted_transaction_time_window).unwrap();
    // Note: filtering out node error means that we might be ignoring failures.
    let blocks = get_blocks_from(start, &web3)
        .await
        .filter_map(|x| async { x.ok() });

    let txs = blocks
        .then(|block| async {
            tracing::info!(
                "processing block {:?} at timestamp {:?}, hash {:?}",
                block.number,
                block.timestamp,
                block.hash
            );
            let time = Utc.timestamp(block.timestamp.as_u64() as i64, 0);
            extract_transactions_to(block, settlement_contract.address(), &web3)
                .await
                .map(move |receipt| (time, receipt))
        })
        .flatten();
    pin_mut!(txs);

    let mut latest_alert_time = Utc.timestamp(0, 0);
    while let Some((time, settlement)) = txs.next().await {
        if settlement.is_none() {
            tracing::warn!("failed to retrieve settlement receipt");
            continue;
        };
        let settlement = settlement.unwrap();
        tracing::info!(
            "found a transaction to the settlement contract with hash {:?} at block {:?}, status {:?}",
            settlement.transaction_hash,
            settlement.block_number,
            settlement.status
        );
        let success = settlement
            .status
            .map(|status| status.as_u64() == 1_u64)
            .unwrap_or(false);
        counter.push_settlement(time, success);
        let Count { total, failures } = counter.latest_count();
        let now = Utc::now();
        if total >= MIN_TRANSACTIONS
            && (failures as f64) / (total as f64) > args.reverted_transaction_alert_threshold
            && latest_alert_time + chrono::Duration::minutes(MINUTES_BETWEEN_ALERTS) <= now
        {
            latest_alert_time = now;
            tracing::error!(
                "{}/{} transactions failed in the previous {} seconds",
                failures,
                total,
                args.reverted_transaction_time_window.as_secs(),
            );
        }
        counter.prune();
    }
}
