use anyhow::{anyhow, Context as _, Result};
use futures::{Stream, StreamExt};
use primitive_types::H256;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::watch;
use web3::{
    types::{BlockId, BlockNumber},
    Transport, Web3,
};

pub type Block = web3::types::Block<H256>;

const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Creates a clonable stream that yields the current block whenever it changes and a future
/// driving the updates of current block.
///
/// The stream is not guaranteed to yield *every* block individually without gaps but it does yield
/// the newest block whenever it changes. In practice this means that if the node changes the
/// current block in quick succession we might only observe the last block, skipping some blocks in
/// between.
///
/// The future runs until all associated streams have been dropped.
pub fn current_block_stream(
    web3: Web3<impl Transport>,
) -> (
    impl Stream<Item = Block> + Clone + Unpin,
    impl Future<Output = ()>,
) {
    let (sender, receiver) = watch::channel(None);

    let stream = CurrentBlockStream { receiver };

    let update_future = async move {
        let mut previous_hash = H256::default();
        loop {
            tokio::time::delay_for(POLL_INTERVAL).await;
            let block = match current_block(&web3).await {
                Ok(block) => block,
                Err(err) => {
                    tracing::warn!("failed to get current block: {:?}", err);
                    continue;
                }
            };
            let hash = match block.hash {
                Some(hash) => hash,
                None => {
                    tracing::warn!("missing hash");
                    continue;
                }
            };
            if hash == previous_hash {
                continue;
            }
            if sender.broadcast(Some(block)).is_err() {
                break;
            }
            previous_hash = hash;
        }
    };

    (stream, update_future)
}

#[derive(Clone)]
struct CurrentBlockStream {
    receiver: watch::Receiver<Option<Block>>,
}

impl CurrentBlockStream {
    async fn next(&mut self) -> Option<Block> {
        loop {
            // recv returns Option<Option<Block>>. If the outer option is None then the sender has
            // been dropped in which case the stream ends. If the inner option is None then this is
            // because we have fetched the initial default value so we loop and try again.
            if let Some(block) = self.receiver.recv().await? {
                return Some(block);
            }
        }
    }
}

impl Stream for CurrentBlockStream {
    type Item = Block;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let future = self.get_mut().next();
        futures::pin_mut!(future);
        future.poll(cx)
    }
}

async fn current_block(web3: &Web3<impl Transport>) -> Result<Block> {
    web3.eth()
        .block(BlockId::Number(BlockNumber::Latest))
        .await
        .context("failed to get current block")?
        .ok_or_else(|| anyhow!("no current block"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    // cargo test current_block -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn mainnet() {
        let node = "https://dev-openethereum.mainnet.gnosisdev.com";
        let transport = web3::transports::Http::new(node).unwrap();
        let web3 = Web3::new(transport);
        let (mut stream, future) = current_block_stream(web3);
        let handle = tokio::task::spawn(future);
        for _ in 0..3 {
            let block = stream.next().await.unwrap();
            println!("new block number {}", block.number.unwrap().as_u64());
        }
        println!("dropping stream which should cause the future to exit");
        std::mem::drop(stream);
        handle.await.unwrap();
    }
}
