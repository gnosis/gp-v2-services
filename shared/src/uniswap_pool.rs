use std::collections::HashSet;

use contracts::{UniswapV2Factory, UniswapV2Pair};
use ethcontract::{batch::CallBatch, Http, Web3, H160, U256};
use num::rational::Rational;
use web3::signing::keccak256;

use hex_literal::hex;
use model::TokenPair;

const UNISWAP_PAIR_INIT_CODE: [u8; 32] =
    hex!("96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f");

const HONEYSWAP_PAIR_INIT_CODE: [u8; 32] =
    hex!("3f88503e8580ab941773b59034fb4b2a63e86dbc031b3633a925533ad3ed2b93");
const MAX_BATCH_SIZE: usize = 100;

#[async_trait::async_trait]
pub trait PoolFetching: Send + Sync {
    async fn fetch(&self, token_pairs: HashSet<TokenPair>) -> Vec<Pool>;
}

#[derive(Clone, Hash)]
pub struct Pool {
    pub tokens: TokenPair,
    pub reserves: (u128, u128),
    pub fee: Rational,
}

impl Pool {
    pub fn uniswap(tokens: TokenPair, reserves: (u128, u128)) -> Self {
        Self {
            tokens,
            reserves,
            fee: Rational::new(3, 1000),
        }
    }

    /// Given an input amount and token, returns the maximum output amount and address of the other asset.
    pub fn get_amount_out(&self, token_in: H160, amount_in: U256) -> (U256, H160) {
        // https://github.com/Uniswap/uniswap-v2-periphery/blob/master/contracts/libraries/UniswapV2Library.sol#L43
        let (reserve_in, reserve_out, token_out) = if token_in == self.tokens.get().0 {
            (
                U256::from(self.reserves.0),
                U256::from(self.reserves.1),
                self.tokens.get().1,
            )
        } else {
            assert!(token_in == self.tokens.get().1, "Token not part of pool");
            (
                U256::from(self.reserves.1),
                U256::from(self.reserves.0),
                self.tokens.get().0,
            )
        };

        let amount_in_with_fee = amount_in * (self.fee.denom() - self.fee.numer());
        let numerator = amount_in_with_fee * reserve_out;
        let denominator = reserve_in * self.fee.denom() + amount_in_with_fee;
        (numerator / denominator, token_out)
    }

    /// Given an output amount and token, returns a required input amount and address of the other asset. Returns None if out amount is larger than reserve.
    pub fn get_amount_in(&self, token_out: H160, amount_out: U256) -> Option<(U256, H160)> {
        // https://github.com/Uniswap/uniswap-v2-periphery/blob/master/contracts/libraries/UniswapV2Library.sol#L53
        let (reserve_out, reserve_in, token_in) = if token_out == self.tokens.get().0 {
            (
                U256::from(self.reserves.0),
                U256::from(self.reserves.1),
                self.tokens.get().1,
            )
        } else {
            assert!(token_out == self.tokens.get().1, "Token not part of pool");
            (
                U256::from(self.reserves.1),
                U256::from(self.reserves.0),
                self.tokens.get().0,
            )
        };

        if amount_out >= reserve_out {
            return None;
        }

        let numerator = reserve_in * amount_out * self.fee.denom();
        let denominator = (reserve_out - amount_out) * (self.fee.denom() - self.fee.numer());
        Some(((numerator / denominator) + 1, token_in))
    }
}

pub struct PoolFetcher {
    pub factory: UniswapV2Factory,
    pub web3: Web3<Http>,
    pub chain_id: u64,
}

#[async_trait::async_trait]
impl PoolFetching for PoolFetcher {
    async fn fetch(&self, token_pairs: HashSet<TokenPair>) -> Vec<Pool> {
        let mut batch = CallBatch::new(self.web3.transport());
        let futures = token_pairs
            .into_iter()
            .map(|pair| {
                let uniswap_pair_address =
                    pair_address(&pair, self.factory.address(), self.chain_id);
                let pair_contract =
                    UniswapV2Pair::at(&self.factory.raw_instance().web3(), uniswap_pair_address);

                (pair, pair_contract.get_reserves().batch_call(&mut batch))
            })
            .collect::<Vec<_>>();

        batch.execute_all(MAX_BATCH_SIZE).await;

        let mut results = Vec::with_capacity(futures.len());
        for (pair, future) in futures {
            if let Ok(result) = future.await {
                results.push(Pool::uniswap(pair, (result.0, result.1)));
            }
        }
        results
    }
}

fn pair_address(pair: &TokenPair, factory: H160, chain_id: u64) -> H160 {
    // https://uniswap.org/docs/v2/javascript-SDK/getting-pair-addresses/
    let mut packed = [0u8; 40];
    packed[0..20].copy_from_slice(pair.get().0.as_fixed_bytes());
    packed[20..40].copy_from_slice(pair.get().1.as_fixed_bytes());
    let salt = keccak256(&packed);
    let init_hash = match chain_id {
        100 => HONEYSWAP_PAIR_INIT_CODE,
        _ => UNISWAP_PAIR_INIT_CODE,
    };
    create2(factory, &salt, &init_hash)
}

fn create2(address: H160, salt: &[u8; 32], init_hash: &[u8; 32]) -> H160 {
    let mut preimage = [0xff; 85];
    preimage[1..21].copy_from_slice(address.as_fixed_bytes());
    preimage[21..53].copy_from_slice(salt);
    preimage[53..85].copy_from_slice(init_hash);
    H160::from_slice(&keccak256(&preimage)[12..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create2_mainnet() {
        // https://info.uniswap.org/pair/0x3e8468f66d30fc99f745481d4b383f89861702c6
        let mainnet_factory = H160::from_slice(&hex!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"));
        let pair = TokenPair::new(
            H160::from_slice(&hex!("6810e776880c02933d47db1b9fc05908e5386b96")),
            H160::from_slice(&hex!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2")),
        )
        .unwrap();
        assert_eq!(
            pair_address(&pair, mainnet_factory, 1),
            H160::from_slice(&hex!("3e8468f66d30fc99f745481d4b383f89861702c6"))
        );
    }

    #[test]
    fn test_create2_xdai() {
        // https://info.honeyswap.org/pair/0x4505b262dc053998c10685dc5f9098af8ae5c8ad
        let mainnet_factory = H160::from_slice(&hex!("A818b4F111Ccac7AA31D0BCc0806d64F2E0737D7"));
        let pair = TokenPair::new(
            H160::from_slice(&hex!("71850b7e9ee3f13ab46d67167341e4bdc905eef9")),
            H160::from_slice(&hex!("e91d153e0b41518a2ce8dd3d7944fa863463a97d")),
        )
        .unwrap();
        assert_eq!(
            pair_address(&pair, mainnet_factory, 100),
            H160::from_slice(&hex!("4505b262dc053998c10685dc5f9098af8ae5c8ad"))
        );
    }

    #[test]
    fn test_get_amounts_out() {
        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(2);

        // Even Pool
        let pool = Pool::uniswap(TokenPair::new(sell_token, buy_token).unwrap(), (100, 100));
        assert_eq!(
            pool.get_amount_out(sell_token, 10.into()),
            (9.into(), buy_token)
        );
        assert_eq!(
            pool.get_amount_out(sell_token, 100.into()),
            (49.into(), buy_token)
        );
        assert_eq!(
            pool.get_amount_out(sell_token, 1000.into()),
            (90.into(), buy_token)
        );

        //Uneven Pool
        let pool = Pool::uniswap(TokenPair::new(sell_token, buy_token).unwrap(), (200, 50));
        assert_eq!(
            pool.get_amount_out(sell_token, 10.into()),
            (2.into(), buy_token)
        );
        assert_eq!(
            pool.get_amount_out(sell_token, 100.into()),
            (16.into(), buy_token)
        );
        assert_eq!(
            pool.get_amount_out(sell_token, 1000.into()),
            (41.into(), buy_token)
        );

        // Large Numbers
        let pool = Pool::uniswap(
            TokenPair::new(sell_token, buy_token).unwrap(),
            (u128::max_value(), u128::max_value()),
        );
        assert_eq!(
            pool.get_amount_out(sell_token, 10u128.pow(20).into()),
            (99_699_999_999_999_999_970u128.into(), buy_token)
        );
    }

    #[test]
    fn test_get_amounts_in() {
        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(2);

        // Even Pool
        let pool = Pool::uniswap(TokenPair::new(sell_token, buy_token).unwrap(), (100, 100));
        assert_eq!(
            pool.get_amount_in(buy_token, 10.into()),
            Some((12.into(), sell_token))
        );
        assert_eq!(
            pool.get_amount_in(buy_token, 99.into()),
            Some((9930.into(), sell_token))
        );

        // Buying more than possible
        assert_eq!(pool.get_amount_in(buy_token, 100.into()), None);
        assert_eq!(pool.get_amount_in(buy_token, 1000.into()), None);

        //Uneven Pool
        let pool = Pool::uniswap(TokenPair::new(sell_token, buy_token).unwrap(), (200, 50));
        assert_eq!(
            pool.get_amount_in(buy_token, 10.into()),
            Some((51.into(), sell_token))
        );
        assert_eq!(
            pool.get_amount_in(buy_token, 49.into()),
            Some((9830.into(), sell_token))
        );

        // Large Numbers
        let pool = Pool::uniswap(
            TokenPair::new(sell_token, buy_token).unwrap(),
            (u128::max_value(), u128::max_value()),
        );
        assert_eq!(
            pool.get_amount_in(buy_token, 10u128.pow(20).into()),
            Some((100_300_902_708_124_373_149u128.into(), sell_token))
        );
    }
}
