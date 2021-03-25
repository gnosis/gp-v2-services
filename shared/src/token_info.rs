use anyhow::{anyhow, Result};
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
    pub decimals: u8,
}

pub struct TokenInfoFetcher {
    pub web3: Web3<Http>,
}

#[automock]
#[async_trait]
pub trait TokenInfoFetching: Send + Sync {
    /// Retrieves all token information.
    /// Default implementation calls get_token_info for each token and ignores errors.
    async fn get_token_infos(&self, addresses: &[H160]) -> HashMap<H160, Result<TokenInfo>>;
}

#[async_trait]
impl TokenInfoFetching for TokenInfoFetcher {
    async fn get_token_infos(&self, addresses: &[H160]) -> HashMap<H160, Result<TokenInfo>> {
        let web3 = Web3::new(self.web3.transport().clone());
        let mut batch = CallBatch::new(self.web3.transport());
        let futures = addresses
            .into_iter()
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
                (
                    *r.0,
                    if r.1.is_ok() {
                        Ok(TokenInfo {
                            decimals: r.1.unwrap(),
                        })
                    } else {
                        Err(anyhow!("Failed to collect token info."))
                    },
                )
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
    async fn get_token_infos(&self, addresses: &[H160]) -> HashMap<H160, Result<TokenInfo>> {
        let mut cache = self.cache.lock().await;

        // Compute set of requested addresses that are not in cache.
        let to_fetch: Vec<H160> = addresses
            .iter()
            .filter(|address| !cache.contains_key(address))
            .cloned()
            .collect();

        // Fetch token infos not yet in cache.
        let fetched = self.inner.get_token_infos(to_fetch.as_slice()).await;

        // Add valid token infos to cache.
        // NOTE: We could also store address->Result<TokenInfo> in the cache
        // to avoid refetching token infos for which there is a non-transient error.
        cache.extend(fetched.into_iter().filter_map(|ati| {
            if ati.1.is_ok() {
                Some((ati.0, ati.1.unwrap()))
            } else {
                None
            }
        }));

        // Return token infos from the cache.
        addresses
            .into_iter()
            .map(|address| {
                (
                    *address,
                    cache
                        .get(address)
                        .map(|ti| ti.clone())
                        .ok_or(anyhow!("Failed to collect token info.")),
                )
            })
            .collect()
    }
}
