use anyhow::Result;
use contracts::{GPv2Settlement, UniswapV2Factory, UniswapV2Pair, UniswapV2Router02};
use ethcontract::{batch::CallBatch, Http, Web3};
use hex_literal::hex;
use model::TokenPair;
use num::rational::Rational;
use primitive_types::{H160, U256};
use std::collections::{hash_map::Entry, HashMap};
use std::sync::Arc;
use web3::signing::keccak256;

const UNISWAP_PAIR_INIT_CODE: [u8; 32] =
    hex!("96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f");

use crate::interactions::UniswapInteraction;
use crate::settlement::Interaction;

use super::{AmmOrder, AmmSettlementHandling, LimitOrder};

pub struct UniswapLiquidity {
    inner: Arc<Inner>,
}

struct Inner {
    factory: UniswapV2Factory,
    router: UniswapV2Router02,
    gpv2_settlement: GPv2Settlement,
    web3: Web3<Http>,
}

impl UniswapLiquidity {
    pub fn new(
        factory: UniswapV2Factory,
        router: UniswapV2Router02,
        gpv2_settlement: GPv2Settlement,
        web3: Web3<Http>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                factory,
                router,
                gpv2_settlement,
                web3,
            }),
        }
    }

    /// Given a list of offchain orders returns the list of AMM liquidity to be considered
    pub async fn get_liquidity(
        &self,
        offchain_orders: impl Iterator<Item = &LimitOrder> + Send + Sync,
    ) -> Result<Vec<AmmOrder>> {
        // TODO: include every token with ETH pair in the pools
        let mut pools = HashMap::new();
        let mut batch = CallBatch::new(self.inner.web3.transport());
        for order in offchain_orders {
            let pair =
                TokenPair::new(order.buy_token, order.sell_token).expect("buy token = sell token");
            let vacant = match pools.entry(pair) {
                Entry::Occupied(_) => continue,
                Entry::Vacant(vacant) => vacant,
            };
            let uniswap_pair_address = pair_address(&pair, self.inner.factory.address());
            let pair_contract = UniswapV2Pair::at(
                &self.inner.factory.raw_instance().web3(),
                uniswap_pair_address,
            );

            let future = pair_contract.get_reserves().batch_call(&mut batch);
            vacant.insert(future);
        }
        batch.execute_all().await?;

        let mut result = Vec::new();
        for (pair, future) in pools {
            match future.await {
                Ok(reserves) => result.push(AmmOrder {
                    tokens: pair,
                    reserves: (reserves.0, reserves.1),
                    fee: Rational::new_raw(3, 1000),
                    settlement_handling: self.inner.clone(),
                }),
                Err(err) => {
                    tracing::warn!(
                        "Couldn't get reserves of Uniswap Pool for pair {:?} - {}",
                        pair,
                        err
                    );
                }
            };
        }
        Ok(result)
    }
}

impl AmmSettlementHandling for Inner {
    fn settle(&self, input: (H160, U256), output: (H160, U256)) -> Vec<Box<dyn Interaction>> {
        vec![Box::new(UniswapInteraction {
            contract: self.router.clone(),
            settlement: self.gpv2_settlement.clone(),
            // TODO(fleupold) Only set allowance if we need to
            set_allowance: true,
            amount_in: input.1,
            amount_out_min: output.1,
            token_in: input.0,
            token_out: output.0,
        })]
    }
}

fn pair_address(pair: &TokenPair, factory: H160) -> H160 {
    // https://uniswap.org/docs/v2/javascript-SDK/getting-pair-addresses/
    let mut packed = [0u8; 40];
    packed[0..20].copy_from_slice(pair.get().0.as_fixed_bytes());
    packed[20..40].copy_from_slice(pair.get().1.as_fixed_bytes());
    let salt = keccak256(&packed);
    create2(factory, &salt, &UNISWAP_PAIR_INIT_CODE)
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
    fn test_create2() {
        // https://info.uniswap.org/pair/0x3e8468f66d30fc99f745481d4b383f89861702c6
        let mainnet_factory = H160::from_slice(&hex!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"));
        let pair = TokenPair::new(
            H160::from_slice(&hex!("6810e776880c02933d47db1b9fc05908e5386b96")),
            H160::from_slice(&hex!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2")),
        )
        .unwrap();
        assert_eq!(
            pair_address(&pair, mainnet_factory),
            H160::from_slice(&hex!("3e8468f66d30fc99f745481d4b383f89861702c6"))
        );
    }
}
