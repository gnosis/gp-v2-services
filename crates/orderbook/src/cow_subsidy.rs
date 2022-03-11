use std::{sync::Mutex, time::Duration};

use anyhow::Result;
use cached::{Cached, TimedSizedCache};
use contracts::ERC20;
use primitive_types::{H160, U256};

const CACHE_SIZE: usize = 10_000;
const CACHE_LIFESPAN: Duration = Duration::from_secs(60 * 60);

/// Check whether a user qualifies for an extra fee subsidy because they own enough cow token.
#[async_trait::async_trait]
pub trait CowSubsidy: Send + Sync + 'static {
    async fn cow_subsidy_factor(&self, user: H160) -> Result<f64>;
}

pub struct FixedCowSubsidy(pub f64);

impl Default for FixedCowSubsidy {
    fn default() -> Self {
        Self(1.0)
    }
}

#[async_trait::async_trait]
impl CowSubsidy for FixedCowSubsidy {
    /// On success returns a factor of how much of the usual fee an account has to pay based on the
    /// amount of COW tokens it owns.
    async fn cow_subsidy_factor(&self, _: H160) -> Result<f64> {
        Ok(self.0)
    }
}

#[derive(Copy, Clone, Debug)]
pub struct SubsidyTier {
    /// How many base units of COW someone needs to own to qualify for this subsidy tier.
    threshold: U256,
    /// How much of the usual fee a user in this tier has to pay.
    fee_factor: f64,
}

impl std::str::FromStr for SubsidyTier {
    type Err = anyhow::Error;
    fn from_str(tier: &str) -> Result<Self, Self::Err> {
        let mut parts = tier.split(':');
        let threshold = parts.next().ok_or(anyhow::anyhow!("missing threshold"))?;
        let threshold: f64 = threshold
            .parse()
            .map_err(|_| anyhow::anyhow!("can not parse threshold {} as f64", threshold))?;

        let fee_factor = parts.next().ok_or(anyhow::anyhow!("missing fee factor"))?;
        let fee_factor: f64 = fee_factor
            .parse()
            .map_err(|_| anyhow::anyhow!("can not parse fee factor {} as f64", fee_factor))?;

        if parts.next().is_some() {
            anyhow::bail!("too many arguments for subsidy tier");
        }

        Ok(SubsidyTier::new(
            U256::from_f64_lossy(1e18 * threshold),
            fee_factor,
        ))
    }
}

impl SubsidyTier {
    pub fn new(threshold: U256, fee_factor: f64) -> Self {
        assert!(fee_factor <= 1.0);
        assert!(fee_factor >= 0.0);
        Self {
            threshold,
            fee_factor,
        }
    }
}

pub struct CowSubsidyImpl {
    cow_token: ERC20,
    // sorted by threshold in increasing order, no duplicated thresholds
    subsidy_tiers: Vec<SubsidyTier>,
    cache: Mutex<TimedSizedCache<H160, f64>>,
}

#[async_trait::async_trait]
impl CowSubsidy for CowSubsidyImpl {
    async fn cow_subsidy_factor(&self, user: H160) -> Result<f64> {
        if let Some(subsidy_factor) = self.cache.lock().unwrap().cache_get(&user).copied() {
            return Ok(subsidy_factor);
        }
        let subsidy_factor = self.subsidy_factor_uncached(user).await?;
        self.cache.lock().unwrap().cache_set(user, subsidy_factor);
        Ok(subsidy_factor)
    }
}

impl CowSubsidyImpl {
    pub fn new(cow_token: ERC20, mut tiers: Vec<SubsidyTier>) -> Self {
        // NOTE: A long caching time might bite us should we ever start advertising that people can
        // buy COW to reduce their fees. `CACHE_LIFESPAN` would have to pass after buying COW to
        // qualify for the subsidy.
        let cache = TimedSizedCache::with_size_and_lifespan_and_refresh(
            CACHE_SIZE,
            CACHE_LIFESPAN.as_secs(),
            false,
        );
        tiers.sort_by_key(|tier| tier.threshold);
        tiers.dedup_by_key(|tier| tier.threshold);
        Self {
            cow_token,
            subsidy_tiers: tiers,
            cache: Mutex::new(cache),
        }
    }

    async fn subsidy_factor_uncached(&self, user: H160) -> Result<f64> {
        let balance = self.cow_token.balance_of(user).call().await?;
        Ok(lookup_subsidy_factor(balance, &self.subsidy_tiers))
    }
}

/// Looks up a subdidy factor in a list of `SubsidyTier`s sorted by their threshold.
/// In case there are multiple factors associated to the same threshold the last one will be taken.
/// If the given balance would not qualify for a `SubsidyTier` a fee factor of 1.0 will be returned
/// which is equivalent to no subsidy.
fn lookup_subsidy_factor(balance: U256, tiers: &[SubsidyTier]) -> f64 {
    // TODO: assert that tiers are sorted when `is_sorted_by_key` gets stabilized:
    // https://doc.rust-lang.org/std/primitive.slice.html#method.is_sorted_by_key
    tiers
        .iter()
        .filter(|tier| tier.threshold <= balance)
        .map(|tier| tier.fee_factor)
        .last()
        .unwrap_or(1.0)
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
        let subsidy = CowSubsidyImpl::new(
            token,
            vec![SubsidyTier::new(U256::from_f64_lossy(1e18), 0.5)],
        );
        for i in 0..2 {
            let user = H160::from_low_u64_be(i);
            let result = subsidy.cow_subsidy_factor(user).await;
            println!("{:?} {:?}", user, result);
        }
    }

    #[test]
    fn subsidy_factors() {
        let tiers = Vec::default();
        assert_eq!(
            lookup_subsidy_factor(U256::MAX, &tiers).to_bits(),
            1.0f64.to_bits()
        );

        let tiers = vec![
            SubsidyTier::new(1.into(), 0.9),
            SubsidyTier::new(1.into(), 0.8),
            SubsidyTier::new(2.into(), 0.7),
            SubsidyTier::new(U256::MAX, 0.0),
        ];
        assert_eq!(
            lookup_subsidy_factor(0.into(), &tiers).to_bits(),
            1.0f64.to_bits()
        );
        // the last factor of duplicated thresholds will be taken
        assert_eq!(
            lookup_subsidy_factor(1.into(), &tiers).to_bits(),
            0.8f64.to_bits()
        );
        assert_eq!(
            lookup_subsidy_factor(2.into(), &tiers).to_bits(),
            0.7f64.to_bits()
        );
        assert_eq!(
            lookup_subsidy_factor(U256::MAX, &tiers).to_bits(),
            0.0f64.to_bits()
        );
    }
}
