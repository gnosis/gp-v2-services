use anyhow::{Context, Result};
use contracts::{GPv2Settlement, UniswapV2Factory, UniswapV2Router02};
use model::TokenPair;
use num_rational::Rational;
use primitive_types::{H160, U256};
use std::collections::{hash_map::Entry, HashMap};
use std::sync::Arc;

use crate::interactions::UniswapInteraction;
use crate::settlement::Interaction;
use crate::uniswap;
use crate::uniswap::Pool;

use super::{AmmOrder, AmmSettlementHandling, Liquidity, LiquiditySource};

pub struct UniswapLiquidity {
    inner: Arc<Inner>,
}

impl UniswapLiquidity {
    pub fn new(
        factory: UniswapV2Factory,
        router: UniswapV2Router02,
        gpv2_settlement: GPv2Settlement,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                factory,
                router,
                gpv2_settlement,
            }),
        }
    }
}

struct Inner {
    factory: UniswapV2Factory,
    router: UniswapV2Router02,
    gpv2_settlement: GPv2Settlement,
}

#[async_trait::async_trait]
impl LiquiditySource for UniswapLiquidity {
    async fn get_liquidity(
        &self,
        liquidity_so_far: impl Iterator<Item = &Liquidity> + Send + Sync + 'async_trait,
    ) -> Result<Vec<Liquidity>> {
        // TODO: include every token with ETH pair in the pools
        let mut pools = HashMap::new();
        for order in liquidity_so_far.filter_map(|l| match l {
            Liquidity::Limit(order) => Some(order),
            _ => None,
        }) {
            let pair =
                TokenPair::new(order.buy_token, order.sell_token).expect("buy token = sell token");
            let vacant = match pools.entry(pair) {
                Entry::Occupied(_) => continue,
                Entry::Vacant(vacant) => vacant,
            };
            let pool = match uniswap::Pool::from_token_pair(&self.inner.factory, &pair)
                .await
                .context("failed to get uniswap pool")?
            {
                None => continue,
                Some(pool) => pool,
            };
            vacant.insert(pool);
        }
        Ok(pools
            .values()
            .map(|pool| pool_to_amm_order(pool, self.inner.clone()))
            .map(Liquidity::Amm)
            .collect())
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

fn pool_to_amm_order(pool: &Pool, settlement_handling: Arc<dyn AmmSettlementHandling>) -> AmmOrder {
    AmmOrder {
        tokens: pool.token_pair.get(),
        reserves: (pool.reserve0, pool.reserve1),
        fee: Rational::new_raw(3, 1000),
        settlement_handling,
    }
}
