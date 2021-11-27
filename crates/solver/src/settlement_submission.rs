pub mod archer_api;
pub mod archer_settlement;
mod dry_run;
pub mod flashbots_api;
pub mod flashbots_settlement;
mod gas_price_stream;
pub mod retry;
pub mod rpc;

use crate::settlement::Settlement;
use anyhow::Result;
use archer_api::ArcherApi;
use contracts::GPv2Settlement;
use ethcontract::Account;
use flashbots_api::FlashbotsApi;
use gas_estimation::GasPriceEstimating;
use primitive_types::U256;
use shared::metrics::get_metric_storage_registry;
use shared::Web3;
use std::fmt::{Debug, Display, Formatter};
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};
use web3::types::TransactionReceipt;

use self::{
    archer_settlement::ArcherSolutionSubmitter, flashbots_settlement::FlashbotsSolutionSubmitter,
};

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

pub enum TransactionStrategy {
    ArcherNetwork {
        archer_api: ArcherApi,
        max_confirm_time: Duration,
    },
    Flashbots {
        flashbots_api: FlashbotsApi,
        max_confirm_time: Duration,
        flashbots_tip: f64,
    },
    CustomNodes(Vec<Web3>),
    DryRun,
}

pub struct TransactionTimeoutError;

impl Debug for TransactionTimeoutError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("transaction did not get mined in time")
    }
}

impl Display for TransactionTimeoutError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("transaction did not get mined in time")
    }
}

impl std::error::Error for TransactionTimeoutError {}

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
        let metrics: &SettlementSubmissionMetrics =
            SettlementSubmissionMetrics::instance(get_metric_storage_registry()).unwrap();
        // metrics.requests_complete.with_label_values(&["", ""]).inc();

        match &self.transaction_strategy {
            TransactionStrategy::CustomNodes(nodes) => {
                let result = rpc::submit(
                    nodes,
                    account,
                    &self.contract,
                    self.gas_price_estimator.as_ref(),
                    self.target_confirm_time,
                    self.gas_price_cap,
                    settlement,
                    gas_estimate,
                )
                .await;
                process_simple_result(result, "custom_nodes", metrics)
            }
            TransactionStrategy::ArcherNetwork {
                archer_api,
                max_confirm_time,
            } => {
                let submitter = ArcherSolutionSubmitter::new(
                    &self.web3,
                    &self.contract,
                    &account,
                    archer_api,
                    self.gas_price_estimator.as_ref(),
                    self.gas_price_cap,
                )?;
                let result = submitter
                    .submit(
                        self.target_confirm_time,
                        SystemTime::now() + *max_confirm_time,
                        settlement,
                        gas_estimate,
                    )
                    .await;
                process_result(result, "archer_network", metrics)
            }
            TransactionStrategy::Flashbots {
                flashbots_api,
                max_confirm_time,
                flashbots_tip,
            } => {
                let submitter = FlashbotsSolutionSubmitter::new(
                    &self.web3,
                    &self.contract,
                    &account,
                    flashbots_api,
                    self.gas_price_estimator.as_ref(),
                    self.gas_price_cap,
                )?;
                let result = submitter
                    .submit(
                        self.target_confirm_time,
                        SystemTime::now() + *max_confirm_time,
                        settlement,
                        gas_estimate,
                        *flashbots_tip,
                    )
                    .await;
                process_result(result, "flashbots", metrics)
            }
            TransactionStrategy::DryRun => {
                let result = dry_run::log_settlement(account, &self.contract, settlement).await;
                process_simple_result(result, "dry_run", metrics)
            }
        }
    }
}

#[derive(prometheus_metric_storage::MetricStorage, Clone, Debug)]
#[metric(subsystem = "settlement_submission")]
struct SettlementSubmissionMetrics {
    /// Number of completed API requests.
    #[metric(labels("transaction_strategy", "status"))]
    requests_complete: prometheus::CounterVec,
}

fn process_result(
    result: Result<Option<TransactionReceipt>>,
    transaction_strategy: &str,
    metrics: &SettlementSubmissionMetrics,
) -> Result<TransactionReceipt> {
    match result {
        Ok(Some(hash)) => {
            metrics
                .requests_complete
                .with_label_values(&[transaction_strategy, "success"])
                .inc();
            Ok(hash)
        }
        Ok(None) => {
            metrics
                .requests_complete
                .with_label_values(&[transaction_strategy, "timeout"])
                .inc();
            Err(TransactionTimeoutError.into())
        }
        Err(err) => {
            metrics
                .requests_complete
                .with_label_values(&[transaction_strategy, "error"])
                .inc();
            Err(err)
        }
    }
}

fn process_simple_result(
    result: Result<TransactionReceipt>,
    transaction_strategy: &str,
    metrics: &SettlementSubmissionMetrics,
) -> Result<TransactionReceipt> {
    let ok = if result.is_ok() { "success" } else { "error" };
    metrics
        .requests_complete
        .with_label_values(&[transaction_strategy, ok])
        .inc();
    result
}
