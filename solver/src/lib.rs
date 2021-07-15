pub mod driver;
pub mod encoding;
pub mod interactions;
pub mod liquidity;
pub mod liquidity_collector;
pub mod metrics;
pub mod orderbook;
pub mod pending_transactions;
pub mod settlement;
pub mod settlement_simulation;
pub mod settlement_submission;
pub mod solver;
#[cfg(test)]
mod test;
mod util;

use anyhow::Result;
use ethcontract::{contract::MethodDefaults, Account};
use shared::Web3;

pub async fn get_settlement_contract(
    web3: &Web3,
    account: Account,
) -> Result<contracts::GPv2Settlement> {
    let mut settlement_contract = contracts::GPv2Settlement::deployed(&web3).await?;
    *settlement_contract.defaults_mut() = MethodDefaults {
        from: Some(account),
        ..Default::default()
    };
    Ok(settlement_contract)
}
