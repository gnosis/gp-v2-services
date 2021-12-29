use crate::settlement::Settlement;
use anyhow::Result;
use contracts::WETH9;
use primitive_types::U256;

pub async fn optimize_unwrapping<V, VFut, B, BFut>(
    settlement: &mut Settlement,
    settlement_would_succeed: V,
    get_weth_balance: B,
    weth: &WETH9,
    unwrap_factor: f64,
) -> Result<()>
where
    V: Fn(Settlement) -> VFut,
    VFut: futures::Future<Output = bool>,
    B: Fn() -> BFut,
    BFut: futures::Future<Output = Result<U256>>,
{
    let required_eth_payout = settlement.encoder.amount_to_unwrap(weth.address());
    if required_eth_payout.is_zero() {
        return Ok(());
    }

    // simulate settlement without unwrap
    let mut optimized_settlement = settlement.clone();
    optimized_settlement.encoder.drop_unwrap(weth.address());

    if settlement_would_succeed(optimized_settlement.clone()).await {
        tracing::debug!("use internal buffer to unwraps");
        *settlement = optimized_settlement;
        return Ok(());
    }

    let weth_balance = get_weth_balance().await?;
    let amount_to_unwrap = U256::from_f64_lossy(weth_balance.to_f64_lossy() * unwrap_factor);

    if amount_to_unwrap <= required_eth_payout {
        // if we wouldn't unwrap more than required we can leave the settlement as it is
        return Ok(());
    }

    // simulate settlement with way bigger unwrap
    optimized_settlement
        .encoder
        .add_unwrap(crate::interactions::UnwrapWethInteraction {
            weth: weth.clone(),
            amount: amount_to_unwrap,
        });

    if settlement_would_succeed(optimized_settlement.clone()).await {
        tracing::debug!(
            ?amount_to_unwrap,
            "unwrap parts of the settlement contract's WETH buffer"
        );
        *settlement = optimized_settlement;
    }

    Ok(())
}
