use contracts::ERC20;
use ethcontract::{batch::CallBatch, H160, U256};
use futures::future::join_all;
use shared::Web3;
use std::collections::HashMap;

const MAX_BATCH_SIZE: usize = 100;

#[derive(Clone)]
/// Computes the amount of "buffer" ERC20 balance that the http solver can use
/// to offset possible rounding errors in computing the amounts in a solution.
pub struct BufferRetriever {
    web3: Web3,
    settlement_contract: H160,
}

impl BufferRetriever {
    pub fn new(web3: Web3, settlement_contract: H160) -> Self {
        Self {
            web3,
            settlement_contract,
        }
    }
}

#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait BufferRetrieving: Send + Sync {
    async fn get_buffers(&self, tokens: &[H160]) -> HashMap<H160, U256>;
}

#[async_trait::async_trait]
impl BufferRetrieving for BufferRetriever {
    async fn get_buffers(&self, tokens: &[H160]) -> HashMap<H160, U256> {
        let mut batch = CallBatch::new(self.web3.transport());

        let futures = tokens
            .iter()
            .map(|address| {
                let erc20 = ERC20::at(&self.web3, *address);
                erc20
                    .methods()
                    .balance_of(self.settlement_contract)
                    .batch_call(&mut batch)
            })
            .collect::<Vec<_>>();

        batch.execute_all(MAX_BATCH_SIZE).await;

        tokens
            .iter()
            .zip(join_all(futures).await.into_iter())
            .filter_map(|(address, balance)| {
                if balance.is_err() {
                    tracing::debug!(
                        "Failed to fetch settlement contract balance for token {} with error {}",
                        address,
                        balance.as_ref().unwrap_err()
                    );
                }
                Some((*address, balance.ok()?))
            })
            .collect()
    }
}
