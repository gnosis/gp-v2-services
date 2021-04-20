use std::collections::{HashMap, HashSet};

use crate::Web3;
use contracts::{UniswapV2Pair, ERC20};
use ethcontract::{batch::CallBatch, H160, U256};
use model::TokenPair;
use num::{rational::Ratio, BigInt, BigRational, Zero};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::amm_pair_provider::AmmPairProvider;
use crate::current_block::{Block, CurrentBlockStream};

const MAX_BATCH_SIZE: usize = 100;

/// This type denotes `(reserve_a, reserve_b, token_b)` where
/// `reserve_a` refers to the reserve of the excluded token.
type RelativeReserves = (U256, U256, H160);

#[async_trait::async_trait]
pub trait PoolFetching: Send + Sync {
    async fn fetch(&self, token_pairs: HashSet<TokenPair>) -> Vec<Pool>;
}

pub struct FilteredPoolFetcher {
    inner: Box<dyn PoolFetching>,
    unsupported_tokens: HashSet<H160>,
}

impl FilteredPoolFetcher {
    pub fn new(inner: Box<dyn PoolFetching>, unsupported_tokens: HashSet<H160>) -> Self {
        Self {
            inner,
            unsupported_tokens,
        }
    }
}

#[async_trait::async_trait]
impl PoolFetching for FilteredPoolFetcher {
    async fn fetch(&self, token_pairs: HashSet<TokenPair>) -> Vec<Pool> {
        let filtered_pairs = token_pairs
            .into_iter()
            .filter(|pair| !self.unsupported_tokens.iter().any(|t| pair.contains(t)))
            .collect();
        self.inner.fetch(filtered_pairs).await
    }
}

#[derive(Clone, Hash, PartialEq, Debug)]
pub struct Pool {
    pub tokens: TokenPair,
    pub reserves: (u128, u128),
    pub fee: Ratio<u32>,
}

impl Pool {
    pub fn uniswap(tokens: TokenPair, reserves: (u128, u128)) -> Self {
        Self {
            tokens,
            reserves,
            fee: Ratio::new(3, 1000),
        }
    }

    /// Given an input amount and token, returns the maximum output amount and address of the other asset.
    /// Returns None if operation not possible due to arithmetic issues (e.g. over or underflow)
    pub fn get_amount_out(&self, token_in: H160, amount_in: U256) -> Option<(U256, H160)> {
        let (reserve_in, reserve_out, token_out) = self.get_relative_reserves(token_in);
        Some((
            self.amount_out(amount_in, reserve_in, reserve_out)?,
            token_out,
        ))
    }

    /// Given an output amount and token, returns a required input amount and address of the other asset.
    /// Returns None if operation not possible due to arithmetic issues (e.g. over or underflow, reserve too small)
    pub fn get_amount_in(&self, token_out: H160, amount_out: U256) -> Option<(U256, H160)> {
        let (reserve_out, reserve_in, token_in) = self.get_relative_reserves(token_out);
        Some((
            self.amount_in(amount_out, reserve_in, reserve_out)?,
            token_in,
        ))
    }

    /// Given one of the pool's two tokens, returns a tuple containing the `RelativeReserves`
    /// along with the opposite token. That is, the elements returned are (respectively)
    /// - the pool's reserve of token provided
    /// - the reserve of the other token
    /// - the pool's other token
    /// This is essentially a helper method for shuffling values in `get_amount_in` and `get_amount_out`
    fn get_relative_reserves(&self, token: H160) -> RelativeReserves {
        // https://github.com/Uniswap/uniswap-v2-periphery/blob/master/contracts/libraries/UniswapV2Library.sol#L53
        if token == self.tokens.get().0 {
            (
                U256::from(self.reserves.0),
                U256::from(self.reserves.1),
                self.tokens.get().1,
            )
        } else {
            assert_eq!(token, self.tokens.get().1, "Token not part of pool");
            (
                U256::from(self.reserves.1),
                U256::from(self.reserves.0),
                self.tokens.get().0,
            )
        }
    }

    // Given the base token returns the price (as defined in https://www.investopedia.com/terms/c/currencypair.asp#mntl-sc-block_1-0-18)
    // and quote token. E.g. for the EUR/USD pool with balances 100 (base, EUR) & 125 (quote, USD) the spot price is 125/100
    pub fn get_spot_price(&self, base_token: H160) -> Option<(BigRational, H160)> {
        let (reserve_base, reserve_quote, quote_token) = if base_token == self.tokens.get().0 {
            (
                BigInt::from(self.reserves.0),
                BigInt::from(self.reserves.1),
                self.tokens.get().1,
            )
        } else {
            assert_eq!(base_token, self.tokens.get().1, "Token not part of pool");
            (
                BigInt::from(self.reserves.1),
                BigInt::from(self.reserves.0),
                self.tokens.get().0,
            )
        };
        if reserve_base == BigInt::zero() {
            return None;
        }

        Some((BigRational::new(reserve_quote, reserve_base), quote_token))
    }

    fn amount_out(&self, amount_in: U256, reserve_in: U256, reserve_out: U256) -> Option<U256> {
        if amount_in.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
            return None;
        }

        let amount_in_with_fee =
            amount_in.checked_mul(U256::from(self.fee.denom().checked_sub(*self.fee.numer())?))?;
        let numerator = amount_in_with_fee.checked_mul(reserve_out)?;
        let denominator = reserve_in
            .checked_mul(U256::from(*self.fee.denom()))?
            .checked_add(amount_in_with_fee)?;
        numerator.checked_div(denominator)
    }

    fn amount_in(&self, amount_out: U256, reserve_in: U256, reserve_out: U256) -> Option<U256> {
        if amount_out.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
            return None;
        }

        let numerator = reserve_in
            .checked_mul(amount_out)?
            .checked_mul(U256::from(*self.fee.denom()))?;
        let denominator = reserve_out
            .checked_sub(amount_out)?
            .checked_mul(U256::from(self.fee.denom().checked_sub(*self.fee.numer())?))?;
        numerator.checked_div(denominator)?.checked_add(1.into())
    }
}

// Read though Pool Fetcher that keeps previously fetched pools in a cache, which get invalidated whenever there is a new block
pub struct CachedPoolFetcher {
    inner: Box<dyn PoolFetching>,
    cache: Arc<Mutex<(Block, HashMap<TokenPair, Pool>)>>,
    block_stream: CurrentBlockStream,
}

impl CachedPoolFetcher {
    pub fn new(inner: Box<dyn PoolFetching>, block_stream: CurrentBlockStream) -> Self {
        Self {
            inner,
            cache: Arc::new(Mutex::new((block_stream.current_block(), HashMap::new()))),
            block_stream,
        }
    }

    async fn fetch_inner(&self, token_pairs: HashSet<TokenPair>) -> Vec<Pool> {
        let mut cache = self.cache.lock().await;
        let (cache_hits, cache_misses) = token_pairs
            .into_iter()
            .partition::<HashSet<_>, _>(|pair| cache.1.contains_key(pair));
        let cache_results: Vec<_> = cache_hits
            .iter()
            .filter_map(|pair| cache.1.get(pair))
            .cloned()
            .collect();

        let mut inner_results = self.inner.fetch(cache_misses).await;
        for miss in &inner_results {
            cache.1.insert(miss.tokens, miss.clone());
        }
        inner_results.extend(cache_results);
        inner_results
    }

    async fn clear_cache_if_necessary(&self) {
        let mut cache = self.cache.lock().await;
        if cache.0 != self.block_stream.current_block() {
            cache.0 = self.block_stream.current_block();
            cache.1.clear();
        }
    }
}

#[async_trait::async_trait]
impl PoolFetching for CachedPoolFetcher {
    async fn fetch(&self, token_pairs: HashSet<TokenPair>) -> Vec<Pool> {
        self.clear_cache_if_necessary().await;
        self.fetch_inner(token_pairs).await
    }
}

pub struct PoolFetcher {
    pub pair_provider: Arc<dyn AmmPairProvider>,
    pub web3: Web3,
}

#[async_trait::async_trait]
impl PoolFetching for PoolFetcher {
    async fn fetch(&self, token_pairs: HashSet<TokenPair>) -> Vec<Pool> {
        let mut batch = CallBatch::new(self.web3.transport());
        let futures = token_pairs
            .into_iter()
            .map(|pair| {
                let uniswap_pair_address = self.pair_provider.pair_address(&pair);
                let pair_contract = UniswapV2Pair::at(&self.web3, uniswap_pair_address);

                // Fetch ERC20 token balances of the pools to sanity check with reserves
                let token0 = ERC20::at(&self.web3, pair.get().0);
                let token1 = ERC20::at(&self.web3, pair.get().1);

                (
                    pair,
                    pair_contract.get_reserves().batch_call(&mut batch),
                    token0
                        .balance_of(uniswap_pair_address)
                        .batch_call(&mut batch),
                    token1
                        .balance_of(uniswap_pair_address)
                        .batch_call(&mut batch),
                )
            })
            .collect::<Vec<_>>();

        batch.execute_all(MAX_BATCH_SIZE).await;

        let mut results = Vec::with_capacity(futures.len());
        for (pair, get_reserves, token0_balance, token1_balance) in futures {
            let reserves = get_reserves.await;
            let token0_balance = token0_balance.await;
            let token1_balance = token1_balance.await;
            if let (Ok(reserves), Ok(token0_balance), Ok(token1_balance)) =
                (reserves, token0_balance, token1_balance)
            {
                // Some ERC20s (e.g. AMPL) have an elastic supply and can thus reduce the balance of their owners without any transfer or other interaction ("rebase").
                // Such behavior can implicitly change the *k* in the pool's constant product formula. E.g. a pool with 10 USDC and 10 AMPL has k = 100. After a negative
                // rebase the pool's AMPL balance may reduce to 9, thus k should be implicitly updated to 90 (figuratively speaking the pool is undercollateralized).
                // Uniswap pools however only update their reserves upon swaps. Such an "out of sync" pool has numerical issues when computing the right clearing price.
                // Note, that a positive rebase is not problematic as k would increase in this case giving the pool excess in the elastic token (an arbitrageur could
                // benefit by withdrawing the excess from the pool without selling anything).
                // We therefore exclude all pools where the pool's token balance of either token in the pair is less than the cached reserve.
                if U256::from(reserves.0) <= token0_balance
                    && U256::from(reserves.1) <= token1_balance
                {
                    results.push(Pool::uniswap(pair, (reserves.0, reserves.1)));
                }
            }
        }
        results
    }
}

<<<<<<< HEAD:shared/src/pool_fetching.rs
=======
fn pair_address(pair: &TokenPair, factory_address: H160, init_hash: [u8; 32]) -> H160 {
    // https://uniswap.org/docs/v2/javascript-SDK/getting-pair-addresses/
    let mut packed = [0u8; 40];
    packed[0..20].copy_from_slice(pair.get().0.as_fixed_bytes());
    packed[20..40].copy_from_slice(pair.get().1.as_fixed_bytes());
    let salt = keccak256(&packed);
    create2(factory_address, &salt, &init_hash)
}

fn create2(address: H160, salt: &[u8; 32], init_hash: &[u8; 32]) -> H160 {
    let mut preimage = [0xff; 85];
    preimage[1..21].copy_from_slice(address.as_fixed_bytes());
    preimage[21..53].copy_from_slice(salt);
    preimage[53..85].copy_from_slice(init_hash);
    H160::from_slice(&keccak256(&preimage)[12..])
}

>>>>>>> main:shared/src/uniswap_pool.rs
#[cfg(test)]
mod tests {

    use super::*;
    use crate::conversions::big_rational_to_float;
    use assert_approx_eq::assert_approx_eq;
    use ethcontract::H256;
    use maplit::hashset;
    use tokio::sync::watch;

    #[test]
<<<<<<< HEAD:shared/src/pool_fetching.rs
=======
    fn test_create2_mainnet() {
        // https://info.uniswap.org/pair/0x3e8468f66d30fc99f745481d4b383f89861702c6
        let mainnet_factory = H160::from_slice(&hex!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"));
        let pair = TokenPair::new(
            H160::from_slice(&hex!("6810e776880c02933d47db1b9fc05908e5386b96")),
            H160::from_slice(&hex!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2")),
        )
        .unwrap();
        assert_eq!(
            pair_address(&pair, mainnet_factory, UNISWAP_PAIR_INIT_CODE),
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
            pair_address(&pair, mainnet_factory, HONEYSWAP_PAIR_INIT_CODE),
            H160::from_slice(&hex!("4505b262dc053998c10685dc5f9098af8ae5c8ad"))
        );
    }

    #[test]
>>>>>>> main:shared/src/uniswap_pool.rs
    fn test_get_amounts_out() {
        let sell_token = H160::from_low_u64_be(1);
        let buy_token = H160::from_low_u64_be(2);

        // Even Pool
        let pool = Pool::uniswap(TokenPair::new(sell_token, buy_token).unwrap(), (100, 100));
        assert_eq!(
            pool.get_amount_out(sell_token, 10.into()),
            Some((9.into(), buy_token))
        );
        assert_eq!(
            pool.get_amount_out(sell_token, 100.into()),
            Some((49.into(), buy_token))
        );
        assert_eq!(
            pool.get_amount_out(sell_token, 1000.into()),
            Some((90.into(), buy_token))
        );

        //Uneven Pool
        let pool = Pool::uniswap(TokenPair::new(sell_token, buy_token).unwrap(), (200, 50));
        assert_eq!(
            pool.get_amount_out(sell_token, 10.into()),
            Some((2.into(), buy_token))
        );
        assert_eq!(
            pool.get_amount_out(sell_token, 100.into()),
            Some((16.into(), buy_token))
        );
        assert_eq!(
            pool.get_amount_out(sell_token, 1000.into()),
            Some((41.into(), buy_token))
        );

        // Large Numbers
        let pool = Pool::uniswap(
            TokenPair::new(sell_token, buy_token).unwrap(),
            (u128::max_value(), u128::max_value()),
        );
        assert_eq!(
            pool.get_amount_out(sell_token, 10u128.pow(20).into()),
            Some((99_699_999_999_999_999_970u128.into(), buy_token))
        );

        // Overflow
        assert_eq!(pool.get_amount_out(sell_token, U256::max_value()), None);
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

    #[test]
    fn test_spot_price() {
        // Example from https://www.investopedia.com/terms/c/currencypair.asp#mntl-sc-block_1-0-18
        let token_a = H160::from_low_u64_be(1);
        let token_b = H160::from_low_u64_be(2);

        let pool = Pool::uniswap(TokenPair::new(token_a, token_b).unwrap(), (100, 125));
        assert_approx_eq!(
            big_rational_to_float(&pool.get_spot_price(token_a).unwrap().0).unwrap(),
            1.25
        );
        assert_approx_eq!(
            big_rational_to_float(&pool.get_spot_price(token_b).unwrap().0).unwrap(),
            0.8
        );

        assert_eq!(pool.get_spot_price(token_a).unwrap().1, token_b);
        assert_eq!(pool.get_spot_price(token_b).unwrap().1, token_a);

        let pool = Pool::uniswap(TokenPair::new(token_a, token_b).unwrap(), (0, 0));
        assert_eq!(pool.get_spot_price(token_a), None);
    }

    struct FakePoolFetcher(Arc<Mutex<Vec<Pool>>>);
    #[async_trait::async_trait]
    impl PoolFetching for FakePoolFetcher {
        async fn fetch(&self, _: HashSet<TokenPair>) -> Vec<Pool> {
            self.0.lock().await.clone()
        }
    }

    #[tokio::test]
    async fn caching_pool_fetcher() {
        let token_a = H160::from_low_u64_be(1);
        let token_b = H160::from_low_u64_be(2);
        let pair = TokenPair::new(token_a, token_b).unwrap();

        let pools = Arc::new(Mutex::new(vec![Pool::uniswap(pair, (1, 1))]));

        let current_block = Arc::new(std::sync::Mutex::new(Block::default()));

        let (_, receiver) = watch::channel::<Block>(Block::default());
        let block_stream = CurrentBlockStream::new(receiver, current_block.clone());

        let inner = Box::new(FakePoolFetcher(pools.clone()));
        let instance = CachedPoolFetcher::new(inner, block_stream);

        // Read Through
        assert_eq!(
            instance.fetch(hashset!(pair)).await,
            vec![Pool::uniswap(pair, (1, 1))]
        );

        // clear inner to test caching
        pools.lock().await.clear();
        assert_eq!(
            instance.fetch(hashset!(pair)).await,
            vec![Pool::uniswap(pair, (1, 1))]
        );

        // invalidate cache
        *current_block.lock().unwrap() = Block {
            hash: Some(H256::from_low_u64_be(1)),
            ..Default::default()
        };
        assert_eq!(instance.fetch(hashset!(pair)).await, vec![]);
    }
}
