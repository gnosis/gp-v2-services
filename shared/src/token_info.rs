use anyhow::Result;
use async_trait::async_trait;
use contracts::ERC20;
use ethcontract::{Http, Web3, H160};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use mockall::*;

#[cfg_attr(test, derive(Eq, PartialEq))]
#[derive(Copy, Clone, Debug)]
pub struct TokenInfo {
    pub decimals: u8,
}

pub struct TokenInfoFetcher {
    //pub factory: UniswapV2Factory,
    pub web3: Web3<Http>,
    //pub chain_id: u64,
}

#[automock]
#[async_trait]
pub trait TokenInfoFetching: Send + Sync {
    /// Retrieves some token information from a token address.
    async fn get_token_info(&self, address: &H160) -> Result<TokenInfo>;

    /// Retrieves all token information.
    /// Default implementation calls get_token_info for each token and ignores errors.
    async fn get_token_infos(&self, addresses: &[H160]) -> Result<HashMap<H160, TokenInfo>> {
        let mut result = HashMap::new();
        for address in addresses {
            match self.get_token_info(address).await {
                Ok(info) => {
                    result.insert(*address, info);
                }
                Err(err) => tracing::warn!("failed to get token info for {}: {:?}", address, err),
            }
        }
        Ok(result)
    }
}

#[async_trait]
impl TokenInfoFetching for TokenInfoFetcher {
    async fn get_token_info(&self, address: &H160) -> Result<TokenInfo> {
        let web3 = Web3::new(self.web3.transport().clone());
        let erc20 = ERC20::at(&web3, *address);
        let decimals = erc20.methods().decimals().call().await?;
        Ok(TokenInfo { decimals })
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
    async fn get_token_info(&self, address: &H160) -> Result<TokenInfo> {
        let mut cache = self.cache.lock().await;
        if cache.contains_key(address) {
            Ok(cache[address])
        } else {
            let token_info = self.inner.get_token_info(address).await?;
            cache.insert(*address, token_info);
            Ok(token_info)
        }
    }
}
