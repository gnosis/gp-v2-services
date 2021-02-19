use anyhow::{anyhow, Context, Result};
use futures::{stream::FusedStream, Stream};
use primitive_types::H256;
use std::time::Duration;
use web3::{
    types::{BlockId, BlockNumber},
    Transport, Web3,
};

pub type Block = web3::types::Block<H256>;

const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// A stream that yields the current block whenever it changes. This is not guaranteed to yield
/// *every* block individually without gaps but it does yield the newest block whenever it changes.
/// In practice this means that if the node changes the current block in quick succession we might
/// only observe the last block, skipping some blocks in between.
pub fn block_stream(web3: Web3<impl Transport>) -> impl Stream<Item = Block> + FusedStream {
    // We do not use a block filter because filters have been unreliable in the past, do not work
    // with load balancing over multiple nodes and would still need to be polled unless we used web
    // sockets.
    futures::stream::unfold(H256::zero(), move |previous| {
        let web3 = web3.clone();
        async move {
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
                if hash == previous {
                    continue;
                }
                return Some((block, hash));
            }
        }
    })
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
        let stream = block_stream(web3);
        futures::pin_mut!(stream);
        while let Some(block) = stream.next().await {
            println!("new block number {}", block.number.unwrap().as_u64());
        }
    }
}
