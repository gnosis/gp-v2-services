use crate::liquidity::baseline_liquidity::BaselineLiquidity;
use crate::liquidity::{AmmOrder, LimitOrder};
use anyhow::Result;

pub struct BalancerV2Liquidity {
    // Will need more specific fields
// web3: Web3,
}

#[async_trait::async_trait]
impl BaselineLiquidity for BalancerV2Liquidity {
    async fn get_liquidity(
        &self,
        _offchain_orders: &mut (dyn Iterator<Item = &LimitOrder> + Send + Sync),
    ) -> Result<Vec<AmmOrder>> {
        // unimplemented!();
        todo!()
    }
}
