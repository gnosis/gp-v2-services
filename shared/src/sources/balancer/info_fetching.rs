//! Responsible for conversion of a `pool_address` into `WeightedPoolInfo` which is used by the
//! event handler to construct a `RegisteredWeightedPool`.
use crate::{
    sources::balancer::swap::fixed_point::Bfp, sources::uniswap::pool_fetching::MAX_BATCH_SIZE,
    token_info::TokenInfoFetching, Web3,
};
use anyhow::{anyhow, Result};
use contracts::{BalancerV2StablePool, BalancerV2Vault, BalancerV2WeightedPool};
use ethcontract::{batch::CallBatch, Bytes, H160, H256, U256};
use mockall::*;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct CommonPoolInfo {
    pub pool_id: H256,
    pub tokens: Vec<H160>,
    pub scaling_exponents: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct WeightedPoolInfo {
    pub common: CommonPoolInfo,
    pub weights: Vec<Bfp>,
}

#[derive(Clone, Debug)]
pub struct StablePoolInfo {
    pub common: CommonPoolInfo,
    pub amplification_parameter: U256,
}

/// Via `PoolInfoFetcher` (leverages a combination of `Web3` and `TokenInfoFetching`)
/// to recover `WeightedPoolInfo` from a `pool_address` in steps as follows:
/// 1. The `pool_id` is recovered first from the deployed `BalancerV2Vault` contract.
/// 2. With `pool_id` we can BatchCall for `tokens` (just the addresses) and `normalized_weights`
///     Technically, `normalized_weights` could be queried along with `pool_id` in step 1,
///     but batching here or there doesn't make a difference.
/// 3. Finally, the `scaling_exponents` are derived as 18 - decimals (for each the token in the pool)
///     `TokenInfoFetching` is used here since it is optimized for recovering ERC20 info internally.
///
/// Note that all token decimals are required to be returned from `TokenInfoFetching` in order
/// to accurately construct `WeightedPoolInfo`.
pub struct PoolInfoFetcher {
    pub web3: Web3,
    pub token_info_fetcher: Arc<dyn TokenInfoFetching>,
    pub vault: BalancerV2Vault,
}

#[automock]
#[async_trait::async_trait]
pub trait PoolInfoFetching: Send + Sync {
    async fn get_weighted_pool_data(&self, pool_address: H160) -> Result<WeightedPoolInfo>;
    async fn get_stable_pool_data(&self, pool_address: H160) -> Result<StablePoolInfo>;
}

#[async_trait::async_trait]
impl PoolInfoFetching for PoolInfoFetcher {
    /// Could result in ethcontract::{NodeError, MethodError or ContractError}
    async fn get_weighted_pool_data(&self, pool_address: H160) -> Result<WeightedPoolInfo> {
        let mut batch = CallBatch::new(self.web3.transport());
        let pool_contract = BalancerV2WeightedPool::at(&self.web3, pool_address);
        let pool_id = H256::from(pool_contract.methods().get_pool_id().call().await?.0);

        // token_data and weight calls can be batched
        let token_data = self
            .vault
            .methods()
            .get_pool_tokens(Bytes(pool_id.0))
            .batch_call(&mut batch);
        let raw_normalized_weights = pool_contract
            .methods()
            .get_normalized_weights()
            .batch_call(&mut batch);
        batch.execute_all(MAX_BATCH_SIZE).await;

        let tokens = token_data.await?.0;

        let token_decimals = self.token_info_fetcher.get_token_infos(&tokens).await;
        let ordered_decimals = tokens
            .iter()
            .map(|token| token_decimals.get(token).and_then(|t| t.decimals))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow!("all token decimals required to build scaling factors"))?;
        // Note that balancer does not support tokens with more than 18 decimals
        // https://github.com/balancer-labs/balancer-v2-monorepo/blob/ce70f7663e0ac94b25ed60cb86faaa8199fd9e13/pkg/pool-utils/contracts/BasePool.sol#L497-L508
        let scaling_exponents = ordered_decimals
            .iter()
            .map(|decimals| 18u8.checked_sub(*decimals))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow!("token with more than 18 decimals"))?;

        let weights = raw_normalized_weights
            .await?
            .into_iter()
            .map(Bfp::from_wei)
            .collect();
        Ok(WeightedPoolInfo {
            common: CommonPoolInfo {
                pool_id,
                tokens,
                scaling_exponents,
            },
            weights,
        })
    }

    async fn get_stable_pool_data(&self, pool_address: H160) -> Result<StablePoolInfo> {
        let mut batch = CallBatch::new(self.web3.transport());
        let pool_contract = BalancerV2StablePool::at(&self.web3, pool_address);
        let pool_id = H256::from(pool_contract.methods().get_pool_id().call().await?.0);

        // token_data and weight calls can be batched
        let token_data = self
            .vault
            .methods()
            .get_pool_tokens(Bytes(pool_id.0))
            .batch_call(&mut batch);
        let amplification_parameter = pool_contract
            .methods()
            .get_amplification_parameter()
            .batch_call(&mut batch);
        batch.execute_all(MAX_BATCH_SIZE).await;

        let tokens = token_data.await?.0;

        let token_decimals = self.token_info_fetcher.get_token_infos(&tokens).await;
        let ordered_decimals = tokens
            .iter()
            .map(|token| token_decimals.get(token).and_then(|t| t.decimals))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow!("all token decimals required to build scaling factors"))?;
        // Note that balancer does not support tokens with more than 18 decimals
        // https://github.com/balancer-labs/balancer-v2-monorepo/blob/ce70f7663e0ac94b25ed60cb86faaa8199fd9e13/pkg/pool-utils/contracts/BasePool.sol#L497-L508
        let scaling_exponents = ordered_decimals
            .iter()
            .map(|decimals| 18u8.checked_sub(*decimals))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow!("token with more than 18 decimals"))?;

        Ok(StablePoolInfo {
            common: CommonPoolInfo {
                pool_id,
                tokens,
                scaling_exponents,
            },
            amplification_parameter: amplification_parameter.await?.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_info::{MockTokenInfoFetching, TokenInfo};
    use ethcontract_mock::Mock;
    use maplit::hashmap;

    #[tokio::test]
    async fn get_stable_pool_data_err() {
        let mock = Mock::new(49);
        let web3 = mock.web3();

        let vault_contract = mock.deploy(BalancerV2Vault::raw_contract().abi.clone());
        let vault = BalancerV2Vault::at(&web3.clone(), vault_contract.address());
        let stable_pool = mock.deploy(BalancerV2StablePool::raw_contract().abi.clone());

        let pool_id = H256::from_low_u64_be(1);
        let token = H160::from_low_u64_be(1);
        stable_pool
            .expect_call(BalancerV2StablePool::signatures().get_pool_id())
            .returns(Bytes(pool_id.0));
        vault_contract
            .expect_call(BalancerV2Vault::signatures().get_pool_tokens())
            .predicate((predicate::eq(Bytes(pool_id.0)),))
            .returns((vec![token], vec![], U256::zero()));
        stable_pool
            .expect_call(BalancerV2StablePool::signatures().get_amplification_parameter())
            .returns((U256::one(), false, U256::zero()));

        let mut mock_token_info_fetcher = MockTokenInfoFetching::new();
        mock_token_info_fetcher
            .expect_get_token_infos()
            .return_once(move |_| {
                hashmap! {
                    token => TokenInfo { decimals: None },
                }
            });

        let pool_info_fetcher = PoolInfoFetcher {
            web3: web3.clone(),
            token_info_fetcher: Arc::new(mock_token_info_fetcher),
            vault: vault.clone(),
        };

        let pool_info = pool_info_fetcher
            .get_stable_pool_data(stable_pool.address())
            .await;

        assert!(pool_info.is_err());

        let mut mock_token_info_fetcher = MockTokenInfoFetching::new();
        mock_token_info_fetcher
            .expect_get_token_infos()
            .return_once(move |_| {
                hashmap! {
                    token => TokenInfo { decimals: Some(19) },
                }
            });
        let pool_info_fetcher = PoolInfoFetcher {
            web3,
            token_info_fetcher: Arc::new(mock_token_info_fetcher),
            vault,
        };

        let pool_info = pool_info_fetcher
            .get_stable_pool_data(stable_pool.address())
            .await;

        assert!(pool_info.is_err());
    }

    #[tokio::test]
    async fn get_stable_pool_data_ok() {
        let mock = Mock::new(49);
        let web3 = mock.web3();

        let vault_contract = mock.deploy(BalancerV2Vault::raw_contract().abi.clone());
        let vault = BalancerV2Vault::at(&web3.clone(), vault_contract.address());
        let stable_pool = mock.deploy(BalancerV2StablePool::raw_contract().abi.clone());

        let pool_id = H256::from_low_u64_be(1);
        let tokens = vec![H160::from_low_u64_be(1), H160::from_low_u64_be(2)];

        stable_pool
            .expect_call(BalancerV2StablePool::signatures().get_pool_id())
            .returns(Bytes(pool_id.0));
        vault_contract
            .expect_call(BalancerV2Vault::signatures().get_pool_tokens())
            .predicate((predicate::eq(Bytes(pool_id.0)),))
            .returns((tokens.clone(), vec![], U256::zero()));
        stable_pool
            .expect_call(BalancerV2StablePool::signatures().get_amplification_parameter())
            .returns((U256::one(), false, U256::zero()));

        let mut token_info_fetcher = MockTokenInfoFetching::new();
        token_info_fetcher
            .expect_get_token_infos()
            .return_once(move |_| {
                hashmap! {
                    tokens[0] => TokenInfo { decimals: Some(18) },
                    tokens[1] => TokenInfo { decimals: Some(17) },
                }
            });

        let pool_info_fetcher = PoolInfoFetcher {
            web3,
            token_info_fetcher: Arc::new(token_info_fetcher),
            vault,
        };

        let pool_info_result = pool_info_fetcher
            .get_stable_pool_data(stable_pool.address())
            .await;
        println!("{:?}", pool_info_result);
        assert!(pool_info_result.is_ok());

        let pool_info = pool_info_result.unwrap();
        assert_eq!(
            pool_info.common.tokens,
            vec![H160::from_low_u64_be(1), H160::from_low_u64_be(2)]
        );
        assert_eq!(pool_info.common.pool_id, pool_id);
        assert_eq!(pool_info.common.scaling_exponents, vec![0u8, 1u8]);
        assert_eq!(pool_info.amplification_parameter, U256::one());
    }
}
