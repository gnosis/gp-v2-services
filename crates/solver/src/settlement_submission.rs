mod dry_run;
pub mod eden_api;
pub mod flashbots_api;
mod gas_price_stream;
pub mod retry;
pub mod rpc;
pub mod submitter;

use crate::settlement::Settlement;
use anyhow::{bail, Result};
use contracts::GPv2Settlement;
use ethcontract::Account;
use gas_estimation::GasPriceEstimating;
use primitive_types::U256;
use shared::Web3;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use submitter::{Submitter, SubmitterParams, TransactionSubmitting};
use web3::types::TransactionReceipt;

const ESTIMATE_GAS_LIMIT_FACTOR: f64 = 1.2;
const GAS_PRICE_REFRESH_INTERVAL: Duration = Duration::from_secs(15);

pub struct SolutionSubmitter {
    pub web3: Web3,
    pub contract: GPv2Settlement,
    pub gas_price_estimator: Arc<dyn GasPriceEstimating>,
    // for gas price estimation
    pub target_confirm_time: Duration,
    pub gas_price_cap: f64,
    pub transaction_strategy: TransactionStrategy,
}

pub struct StrategyArgs {
    pub submit_api: Box<dyn TransactionSubmitting>,
    pub max_confirm_time: Duration,
    pub retry_interval: Duration,
    pub additional_tip: f64,
}
pub enum TransactionStrategy {
    Eden(StrategyArgs),
    Flashbots(StrategyArgs),
    CustomNodes(Vec<Web3>),
    DryRun,
}

impl SolutionSubmitter {
    /// Submits a settlement transaction to the blockchain, returning the hash
    /// of the successfully mined transaction.
    ///
    /// Errors if the transaction timed out, or an inner error was encountered
    /// during submission.
    pub async fn settle(
        &self,
        settlement: Settlement,
        gas_estimate: U256,
        account: Account,
    ) -> Result<TransactionReceipt> {
        match &self.transaction_strategy {
            TransactionStrategy::CustomNodes(nodes) => {
                rpc::submit(
                    nodes,
                    account,
                    &self.contract,
                    self.gas_price_estimator.as_ref(),
                    self.target_confirm_time,
                    self.gas_price_cap,
                    settlement,
                    gas_estimate,
                )
                .await
            }
            TransactionStrategy::Eden(args) => {
                let submitter = Submitter::new(
                    &self.web3,
                    &self.contract,
                    &account,
                    args.submit_api.as_ref(),
                    self.gas_price_estimator.as_ref(),
                )?;
                let params = SubmitterParams {
                    target_confirm_time: self.target_confirm_time,
                    gas_estimate,
                    gas_price_cap: self.gas_price_cap,
                    deadline: Some(Instant::now() + args.max_confirm_time),
                    additional_miner_tip: Some(args.additional_tip),
                    retry_interval: args.retry_interval,
                };
                let result = submitter.submit(settlement, params).await;
                match result {
                    Ok(Some(hash)) => Ok(hash),
                    Ok(None) => bail!("transaction did not get mined in time"),
                    Err(err) => Err(err),
                }
            }
            TransactionStrategy::Flashbots(args) => {
                let submitter = Submitter::new(
                    &self.web3,
                    &self.contract,
                    &account,
                    args.submit_api.as_ref(),
                    self.gas_price_estimator.as_ref(),
                )?;
                let params = SubmitterParams {
                    target_confirm_time: self.target_confirm_time,
                    gas_estimate,
                    gas_price_cap: self.gas_price_cap,
                    deadline: Some(Instant::now() + args.max_confirm_time),
                    additional_miner_tip: Some(args.additional_tip),
                    retry_interval: args.retry_interval,
                };
                let result = submitter.submit(settlement, params).await;
                match result {
                    Ok(Some(hash)) => Ok(hash),
                    Ok(None) => bail!("transaction did not get mined in time"),
                    Err(err) => Err(err),
                }
            }
            TransactionStrategy::DryRun => {
                dry_run::log_settlement(account, &self.contract, settlement).await
            }
        }
    }
}
