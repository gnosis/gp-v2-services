use crate::liquidity::baseline_liquidity::BaselineLiquidity;
use crate::liquidity::Liquidity;
use anyhow::Result;
use model::TokenPair;
use std::collections::HashSet;

pub struct BalancerV2Liquidity {
    // Will need more specific fields
// web3: Web3,
}

#[async_trait::async_trait]
impl BaselineLiquidity for BalancerV2Liquidity {
    async fn get_liquidity(&self, _pools: HashSet<TokenPair>) -> Result<Vec<Liquidity>> {
        todo!()
    }
}
