use async_trait::async_trait;
use contracts::ERC20;
use ethcontract::{batch::CallBatch, Http, Web3, H160};
use futures::future::join_all;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use mockall::*;

const MAX_BATCH_SIZE: usize = 100;

#[cfg_attr(test, derive(Eq, PartialEq))]
#[derive(Copy, Clone, Debug)]
pub struct TokenInfo {
    pub decimals: Option<u8>,
}

pub struct TokenInfoFetcher {
    pub web3: Web3<Http>,
}

#[automock]
#[async_trait]
pub trait TokenInfoFetching: Send + Sync {
    /// Retrieves all token information.
    /// Default implementation calls get_token_info for each token and ignores errors.
    async fn get_token_infos(&self, addresses: &[H160]) -> HashMap<H160, TokenInfo>;
}

#[async_trait]
impl TokenInfoFetching for TokenInfoFetcher {
    async fn get_token_infos(&self, addresses: &[H160]) -> HashMap<H160, TokenInfo> {
        let web3 = Web3::new(self.web3.transport().clone());
        let mut batch = CallBatch::new(self.web3.transport());
        let futures = addresses
            .iter()
            .map(|address| {
                let erc20 = ERC20::at(&web3, *address);
                erc20.methods().decimals().batch_call(&mut batch)
            })
            .collect::<Vec<_>>();

        batch.execute_all(MAX_BATCH_SIZE).await;

        addresses
            .iter()
            .zip(join_all(futures).await.into_iter())
            .map(|r| {
                if (r.1.is_err()) {
                    tracing::trace!("Failed to fetch token info for token {}", r.0);
                }
                (*r.0, TokenInfo { decimals: r.1.ok() })
            })
            .collect()
    }
}

pub struct CachedTokenInfoFetcher {
    inner: Box<dyn TokenInfoFetching>,
    cache: Arc<Mutex<HashMap<H160, TokenInfo>>>,
}

impl CachedTokenInfoFetcher {
    pub fn new(inner: Box<dyn TokenInfoFetching>) -> Self {
        Self {
            inner,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl TokenInfoFetching for CachedTokenInfoFetcher {
    async fn get_token_infos(&self, addresses: &[H160]) -> HashMap<H160, TokenInfo> {
        let mut cache = self.cache.lock().await;

        // Compute set of requested addresses that are not in cache.
        let to_fetch: Vec<H160> = addresses
            .iter()
            .filter(|address| !cache.contains_key(address))
            .cloned()
            .collect();

        // Fetch token infos not yet in cache.
        let fetched = self.inner.get_token_infos(to_fetch.as_slice()).await;

        // Add token infos to cache.
        cache.extend(fetched);

        // Return token infos from the cache.
        addresses
            .iter()
            .map(|address| (*address, cache[address]))
            .collect()
    }
}
