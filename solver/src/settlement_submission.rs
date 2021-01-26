use std::time::Duration;

use crate::settlement::Settlement;
use anyhow::{anyhow, Context, Result};
use contracts::GPv2Settlement;
use ethcontract::GasPrice;
use gas_estimation::GasPriceEstimating;
use primitive_types::U256;

const MAX_GAS: u32 = 8_000_000;

pub async fn submit(
    settlement: Settlement,
    contract: &GPv2Settlement,
    gas: &dyn GasPriceEstimating,
) -> Result<()> {
    // TODO: use retry transaction sending crate for updating gas prices
    let encoded_interactions = settlement
        .encode_interactions()
        .context("interaction encoding failed")?;
    let encoded_trades = settlement
        .encode_trades()
        .ok_or_else(|| anyhow!("trade encoding failed"))?;
    let settle = || {
        contract
            .settle(
                settlement.tokens(),
                settlement.clearing_prices(),
                encoded_trades.clone(),
                encoded_interactions.clone(),
                Vec::new(),
            )
            .gas(MAX_GAS.into())
    };
    tracing::info!(
        "Settlement call: {}",
        hex::encode(settle().tx.data.expect("data").0),
    );
    let gas_price = gas
        .estimate_with_limits(MAX_GAS.into(), Duration::from_secs(60))
        .await
        .context("failed to get gas price")?;
    settle().call().await.context("settle simulation failed")?;
    settle()
        .gas_price(GasPrice::Value(U256::from_f64_lossy(gas_price)))
        .send()
        .await
        .context("settle execution failed")?;
    Ok(())
}
