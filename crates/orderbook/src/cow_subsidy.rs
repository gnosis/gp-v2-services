use std::{sync::Mutex, time::Duration};

use anyhow::Result;
use cached::{Cached, TimedSizedCache};
use contracts::ERC20;
use primitive_types::{H160, U256};

const CACHE_SIZE: usize = 10000;
const CACHE_LIFESPAN: Duration = Duration::from_secs(60 * 60);

/// Check whether a user qualifies for an extra fee subsidy because they own enough cow token.
#[async_trait::async_trait]
pub trait CowSubsidy: Send + Sync + 'static {
    async fn qualifies_for_subsidy(&self, user: H160) -> Result<bool>;
}

#[derive(Default)]
pub struct NoopCowSubsidy(pub bool);

#[async_trait::async_trait]
impl CowSubsidy for NoopCowSubsidy {
    async fn qualifies_for_subsidy(&self, _: H160) -> Result<bool> {
        Ok(self.0)
    }
}

pub struct CowSubsidyImpl {
    cow_token: ERC20,
    threshold: U256,
    cache: Mutex<TimedSizedCache<H160, bool>>,
}

#[async_trait::async_trait]
impl CowSubsidy for CowSubsidyImpl {
    async fn qualifies_for_subsidy(&self, user: H160) -> Result<bool> {
        if let Some(qualifies) = self.cache.lock().unwrap().cache_get(&user) {
            return Ok(*qualifies);
        }
        let qualifies = self.qualifies_for_subsidy_uncached(user).await?;
        self.cache.lock().unwrap().cache_set(user, qualifies);
        Ok(qualifies)
    }
}

impl CowSubsidyImpl {
    pub fn new(cow_token: ERC20, threshold: U256) -> Self {
        let cache = TimedSizedCache::with_size_and_lifespan_and_refresh(
            CACHE_SIZE,
            CACHE_LIFESPAN.as_secs(),
            false,
        );
        Self {
            cow_token,
            threshold,
            cache: Mutex::new(cache),
        }
    }

    async fn qualifies_for_subsidy_uncached(&self, user: H160) -> Result<bool> {
        let balance = self.cow_token.balance_of(user).call().await?;
        Ok(balance >= self.threshold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex_literal::hex;
    use shared::Web3;

    #[tokio::test]
    #[ignore]
    async fn mainnet() {
        let transport = shared::transport::create_env_test_transport();
        let web3 = Web3::new(transport);
        let token = H160(hex!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"));
        let token = ERC20::at(&web3, token);
        let subsidy = CowSubsidyImpl::new(token, U256::from_f64_lossy(1e18));
        for i in 0..2 {
            let user = H160::from_low_u64_be(i);
            let result = subsidy.qualifies_for_subsidy(user).await;
            println!("{:?} {:?}", user, result);
        }
    }
}
