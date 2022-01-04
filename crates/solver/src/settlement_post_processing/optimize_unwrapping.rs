use crate::settlement::Settlement;
use anyhow::Result;
use contracts::WETH9;
use primitive_types::U256;

/// Tries to do one of 2 optimizations.
/// 1) Drop WETH unwraps and instead pay ETH with the settlment contract's buffer.
/// 2) Top up settlement contract's ETH buffer by unwrapping way more WETH than this settlement
///    needs. This will cause the next few settlements to use optimization 1.
pub async fn optimize_unwrapping<V, VFut, B, BFut>(
    settlement: Settlement,
    settlement_would_succeed: V,
    get_weth_balance: B,
    weth: &WETH9,
    unwrap_factor: f64,
) -> Settlement
where
    V: Fn(Settlement) -> VFut,
    VFut: futures::Future<Output = bool>,
    B: Fn() -> BFut,
    BFut: futures::Future<Output = Result<U256>>,
{
    let required_eth_payout = settlement.encoder.amount_to_unwrap(weth.address());
    if required_eth_payout.is_zero() {
        return settlement;
    }

    // simulate settlement without unwrap
    let mut optimized_settlement = settlement.clone();
    optimized_settlement.encoder.drop_unwrap(weth.address());

    if settlement_would_succeed(optimized_settlement.clone()).await {
        tracing::debug!("use internal buffer to unwraps");
        return optimized_settlement;
    }

    let weth_balance = get_weth_balance().await.unwrap_or_else(|_| U256::zero());
    let amount_to_unwrap = U256::from_f64_lossy(weth_balance.to_f64_lossy() * unwrap_factor);

    if amount_to_unwrap <= required_eth_payout {
        // if we wouldn't unwrap more than required we can leave the settlement as it is
        return settlement;
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
        return optimized_settlement;
    }

    settlement
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interactions::UnwrapWethInteraction;
    use shared::dummy_contract;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn to_wei(base: u128) -> U256 {
        U256::from(base) * U256::from(10).pow(18.into())
    }

    fn settlement_with_unwrap(weth: &WETH9, amount: U256) -> Settlement {
        let mut settlement = Settlement::with_trades(HashMap::default(), Vec::default());
        if !amount.is_zero() {
            settlement.encoder.add_unwrap(UnwrapWethInteraction {
                weth: weth.clone(),
                amount,
            });
        }
        assert_eq!(amount, settlement.encoder.amount_to_unwrap(weth.address()));
        settlement
    }

    #[tokio::test]
    async fn drop_unwrap_if_eth_buffer_is_big_enough() {
        let first_optimization_succeeds = |_: Settlement| async { true };
        let get_weth_balance_succeeds = || async { Ok(U256::zero()) };
        let weth = dummy_contract!(WETH9, [0x42; 20]);

        let settlement = optimize_unwrapping(
            settlement_with_unwrap(&weth, to_wei(1)),
            &first_optimization_succeeds,
            &get_weth_balance_succeeds,
            &weth,
            0.6,
        )
        .await;

        // no unwraps left because we pay 1 ETH from our buffer
        assert_eq!(
            U256::zero(),
            settlement.encoder.amount_to_unwrap(weth.address())
        );
    }

    #[tokio::test]
    async fn bulk_convert_if_weth_buffer_is_big_enough() {
        let successes = Arc::new(Mutex::new(vec![true, false]));
        let second_optimization_succeeds =
            |_: Settlement| async { successes.lock().unwrap().pop().unwrap() };
        let get_weth_balance_succeeds = || async { Ok(to_wei(100)) };
        let weth = dummy_contract!(WETH9, [0x42; 20]);

        let settlement = optimize_unwrapping(
            settlement_with_unwrap(&weth, to_wei(10)),
            &second_optimization_succeeds,
            &get_weth_balance_succeeds,
            &weth,
            0.6,
        )
        .await;

        // we unwrap way more than needed to hopefully drop unwraps on the next few settlements
        assert_eq!(
            to_wei(60),
            settlement.encoder.amount_to_unwrap(weth.address())
        );
    }

    #[tokio::test]
    async fn leave_settlement_unchanged_if_buffers_are_too_small_for_optimizations() {
        // Although we would have enough WETH to cover the ETH payout, we pretend the bulk unwrap
        // would fail anyway. This can happen if the execution_plan of the settlement also tries to
        // use the WETH buffer (In this case more than 10 WETH).
        let no_optimization_succeeds = |_: Settlement| async { false };
        let get_weth_balance_succeeds = || async { Ok(to_wei(100)) };
        let weth = dummy_contract!(WETH9, [0x42; 20]);

        let eth_to_unwrap = to_wei(50);

        let settlement = optimize_unwrapping(
            settlement_with_unwrap(&weth, eth_to_unwrap),
            &no_optimization_succeeds,
            &get_weth_balance_succeeds,
            &weth,
            0.6,
        )
        .await;

        // the settlement has been left unchanged
        assert_eq!(
            eth_to_unwrap,
            settlement.encoder.amount_to_unwrap(weth.address())
        );
    }
}
