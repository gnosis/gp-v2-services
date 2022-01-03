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

        let mut settlement = settlement_with_unwrap(&weth, to_wei(1));

        optimize_unwrapping(
            &mut settlement,
            &first_optimization_succeeds,
            &get_weth_balance_succeeds,
            &weth,
            0.6,
        )
        .await
        .unwrap();

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

        let mut settlement = settlement_with_unwrap(&weth, to_wei(10));
        let unwrap_factor = 0.6;

        optimize_unwrapping(
            &mut settlement,
            &second_optimization_succeeds,
            &get_weth_balance_succeeds,
            &weth,
            unwrap_factor,
        )
        .await
        .unwrap();

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
        let mut settlement = settlement_with_unwrap(&weth, eth_to_unwrap);

        optimize_unwrapping(
            &mut settlement,
            &no_optimization_succeeds,
            &get_weth_balance_succeeds,
            &weth,
            0.6,
        )
        .await
        .unwrap();

        // the settlement has been left unchanged
        assert_eq!(
            eth_to_unwrap,
            settlement.encoder.amount_to_unwrap(weth.address())
        );
    }

    #[tokio::test]
    async fn errors_get_propagated_and_leave_settlement_unchanged() {
        //second optimization would work if the algorithm would get so far
        let successes = Arc::new(Mutex::new(vec![true, false]));
        let second_optimization_succeeds =
            |_: Settlement| async { successes.lock().unwrap().pop().unwrap() };

        let error_message = "can't determine WETH balance";
        let get_weth_balance_fails = || async { Err(anyhow::anyhow!(error_message)) };
        let weth = dummy_contract!(WETH9, [0x42; 20]);

        let eth_to_unwrap = to_wei(60);
        let mut settlement = settlement_with_unwrap(&weth, eth_to_unwrap);

        let optimization_result = optimize_unwrapping(
            &mut settlement,
            &second_optimization_succeeds,
            &get_weth_balance_fails,
            &weth,
            0.6,
        )
        .await;

        assert_eq!(
            error_message,
            optimization_result.as_ref().unwrap_err().to_string(),
        );

        // settlement had been left unchanged because we encountered an error while optimizing it
        assert_eq!(
            eth_to_unwrap,
            settlement.encoder.amount_to_unwrap(weth.address())
        );
    }
}
