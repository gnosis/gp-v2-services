use std::collections::HashMap;

use contracts::ERC20;
use ethcontract::{Account, H160, U256, batch::CallBatch};
use futures::future::join_all;
use shared::Web3;
use super::settlement::Settlement;

const MAX_BATCH_SIZE: usize = 100;


#[async_trait::async_trait]
pub trait InternalLiquidityFetching: Send + Sync {
    async fn fetch(&self, tokens: &Vec<H160>) -> HashMap<H160, U256>;
}
struct InternalLiquidityFetcher {
    web3: Web3,
    designated_solver: Account,
}


#[async_trait::async_trait]
impl InternalLiquidityFetching for InternalLiquidityFetcher {
    async fn fetch(&self, tokens: &Vec<H160>) -> HashMap<H160, U256> {
        let settlement = super::get_settlement_contract(&self.web3, self.designated_solver.clone())
            .await
            .expect("Failed to load deployed GPv2Settlement");

        let mut batch = CallBatch::new(self.web3.transport());
        let futures = tokens
            .iter()
            .map(|token| {
                let token_contract = ERC20::at(&self.web3, *token);
                token_contract
                .balance_of(settlement.address())
                .batch_call(&mut batch)
            })
            .collect::<Vec<_>>();

        batch.execute_all(MAX_BATCH_SIZE).await;

        tokens
            .iter()
            .zip(join_all(futures).await.into_iter())
            .map(|(token, balance)| {
                if balance.is_err() {
                    tracing::trace!("Failed to fetch internal liquidity for token {} - assuming none.", token);                    
                }
                (*token, balance.unwrap_or_default())
            })
            .collect()
    }
}


struct InternalLiquidityManager {
    fetcher: Box<dyn InternalLiquidityFetching>,
    allowed_tokens: Vec<H160>,
}

impl InternalLiquidityManager {
    pub fn transform(settlement: Settlement) -> Settlement {
        settlement.
    }
}