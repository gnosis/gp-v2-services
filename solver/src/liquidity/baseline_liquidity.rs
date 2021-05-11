use crate::liquidity::{AmmOrder, LimitOrder};
use anyhow::Result;

#[async_trait::async_trait]
pub trait BaselineLiquidity {
    async fn get_liquidity(
        &self,
        offchain_orders: &mut (dyn Iterator<Item = &LimitOrder> + Send + Sync),
    ) -> Result<Vec<AmmOrder>>;
}
