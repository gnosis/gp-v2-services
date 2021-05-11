use crate::liquidity::Liquidity;
use anyhow::Result;
use model::TokenPair;
use std::collections::HashSet;

#[async_trait::async_trait]
pub trait BaselineLiquidity {
    async fn get_liquidity(&self, pools: HashSet<TokenPair>) -> Result<Vec<Liquidity>>;
}
