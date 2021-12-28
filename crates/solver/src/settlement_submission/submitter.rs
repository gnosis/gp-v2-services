// Design:
// As in the traditional transaction submission workflow the main work in this module is checking
// the gas price in a loop and updating the transaction when the gas price increases. This differs
// so that we can make use of the property that flashbots transactions do not cost gas if they fail.
// When we detect that the transaction would no longer succeed we stop trying to submit and return
// so that the solver can run again.
// In addition to simulation failure we make use of a deadline after which submission attempts also
// stop. This allows the solver to update and improve a solution even if it hasn't yet become
// invalid.
// We do not know in advance which of our submitted transactions will get mined. Instead of polling
// all of them we only check the account's nonce as an optimization. When this happens all our
// transactions definitely become invalid (even if the transaction came for whatever reason
// from outside) so it is only at that point that we need to check the hashes individually to the
// find the one that got mined (if any).

use super::{SubmissionError, ESTIMATE_GAS_LIMIT_FACTOR};
use crate::{settlement::Settlement, settlement_simulation::settle_method_builder};
use anyhow::{anyhow, Context, Result};
use contracts::GPv2Settlement;
use ethcontract::{contract::MethodBuilder, dyns::DynTransport, transaction::Transaction, Account};
use futures::FutureExt;
use gas_estimation::{EstimatedGasPrice, GasPriceEstimating};
use primitive_types::{H256, U256};
use shared::Web3;
use std::time::{Duration, Instant};
use web3::types::TransactionReceipt;

/// Parameters for transaction submitting
#[derive(Clone, Default)]
pub struct SubmitterParams {
    /// Desired duration to include the transaction in a block
    pub target_confirm_time: Duration, //todo ds change to blocks in the following PR
    /// Estimated gas consumption of a transaction
    pub gas_estimate: U256,
    /// Maximum duration of a single run loop
    pub deadline: Option<Instant>,
    /// Resimulate and resend transaction on every retry_interval seconds
    pub retry_interval: Duration,
}

#[derive(Debug)]
/// Enum used to handle all kind of messages received from implementers of trait TransactionSubmitting
pub enum SubmitApiError {
    InvalidNonce,
    OpenEthereumTooCheapToReplace,
    Other(anyhow::Error),
}

pub struct TransactionHandle(pub H256);

#[async_trait::async_trait]
pub trait TransactionSubmitting {
    /// Submits raw signed transation to the specific network (public mempool, eden, flashbots...).
    /// Returns transaction handle
    async fn submit_raw_transaction(&self, tx: &[u8]) -> Result<TransactionHandle, SubmitApiError>;
    /// Cancels already submitted transaction using the transaction handle
    async fn cancel_transaction(&self, id: &TransactionHandle) -> Result<()>;
}

/// Gas price estimator specialized for sending transactions to the network
pub struct SubmitterGasPriceEstimator<'a> {
    pub inner: &'a dyn GasPriceEstimating,
    /// Boost estimated gas price miner tip in order to increase the chances of a transaction being mined
    pub additional_tip: Option<f64>,
    /// Maximum max_fee_per_gas to pay for a transaction
    pub gas_price_cap: f64,
}

#[async_trait::async_trait]
impl GasPriceEstimating for SubmitterGasPriceEstimator<'_> {
    async fn estimate_with_limits(
        &self,
        gas_limit: f64,
        time_limit: Duration,
    ) -> Result<EstimatedGasPrice> {
        match self.inner.estimate_with_limits(gas_limit, time_limit).await {
            Ok(mut gas_price) if gas_price.cap() <= self.gas_price_cap => {
                // boost miner tip to increase our chances of being included in a block
                if let Some(ref mut eip1559) = gas_price.eip1559 {
                    eip1559.max_priority_fee_per_gas += self.additional_tip.unwrap_or_default();
                }
                Ok(gas_price)
            }
            Ok(gas_price) => Err(anyhow!(
                "gas station gas price {} is larger than cap {}",
                gas_price.cap(),
                self.gas_price_cap
            )),
            Err(err) => Err(err),
        }
    }
}

pub struct Submitter<'a> {
    contract: &'a GPv2Settlement,
    account: &'a Account,
    submit_api: &'a dyn TransactionSubmitting,
    gas_price_estimator: &'a SubmitterGasPriceEstimator<'a>,
}

impl<'a> Submitter<'a> {
    pub fn new(
        contract: &'a GPv2Settlement,
        account: &'a Account,
        submit_api: &'a dyn TransactionSubmitting,
        gas_price_estimator: &'a SubmitterGasPriceEstimator<'a>,
    ) -> Result<Self> {
        Ok(Self {
            contract,
            account,
            submit_api,
            gas_price_estimator,
        })
    }
}

impl<'a> Submitter<'a> {
    /// Submit a settlement to the contract, updating the transaction with gas prices if they increase.
    ///
    /// Only works on mainnet.
    pub async fn submit(
        &self,
        settlement: Settlement,
        params: SubmitterParams,
    ) -> Result<TransactionReceipt, SubmissionError> {
        let nonce = self.nonce().await?;

        tracing::info!("starting solution submission at nonce {}", nonce);

        // Continually simulate and submit transactions
        let mut transactions = Vec::new();
        let submit_future = self.submit_with_increasing_gas_prices_until_simulation_fails(
            settlement,
            nonce,
            &params,
            &mut transactions,
        );

        // Nonce future is used to detect if tx is mined
        let nonce_future = self.wait_for_nonce_to_change(nonce);

        // If specified, deadline future stops submitting when deadline is reached
        let deadline_future = tokio::time::sleep(match params.deadline {
            Some(deadline) => deadline.saturating_duration_since(Instant::now()),
            None => Duration::from_secs(u64::MAX),
        });

        let fallback_result = tokio::select! {
            method_error = submit_future.fuse() => {
                tracing::info!("stopping submission because simulation failed: {:?}", method_error);
                Err(method_error)
            },
            new_nonce = nonce_future.fuse() => {
                tracing::info!("stopping submission because account nonce changed to {}", new_nonce);
                Ok(None)
            },
            _ = deadline_future.fuse() => {
                tracing::info!("stopping submission because deadline has been reached");
                Ok(None)
            },
        };

        // After stopping submission of new transactions we wait for some time to give a potentially
        // mined previously submitted transaction time to propagate to our node.

        // Example:
        // 1. We submit tx to ethereum node, and we start counting 10s pause before new loop iteration.
        // 2. In the meantime, block A gets mined somewhere in the network (not containing our tx)
        // 3. After some time block B is mined somewhere in the network (containing our tx)
        // 4. Our node receives block A.
        // 5. Our 10s is up but our node received only block A because of the delay in block propagation. We simulate tx and it fails, we return back
        // 6. If we don't wait another 20s to receive block B, we wont see mined tx.

        if !transactions.is_empty() {
            const MINED_TX_PROPAGATE_TIME: Duration = Duration::from_secs(20);
            const MINED_TX_CHECK_INTERVAL: Duration = Duration::from_secs(5);
            let tx_to_propagate_deadline = Instant::now() + MINED_TX_PROPAGATE_TIME;

            tracing::info!(
                "waiting up to {} seconds to see if a transaction was mined",
                MINED_TX_PROPAGATE_TIME.as_secs()
            );

            loop {
                if let Some(receipt) =
                    find_mined_transaction(&self.contract.raw_instance().web3(), &transactions)
                        .await
                {
                    tracing::info!("found mined transaction {}", receipt.transaction_hash);
                    return Ok(receipt);
                }
                if Instant::now() + MINED_TX_CHECK_INTERVAL > tx_to_propagate_deadline {
                    break;
                }
                tokio::time::sleep(MINED_TX_CHECK_INTERVAL).await;
            }
        }

        tracing::info!("did not find any mined transaction");
        fallback_result
            .transpose()
            .unwrap_or(Err(SubmissionError::Timeout))
    }

    async fn nonce(&self) -> Result<U256> {
        self.contract
            .raw_instance()
            .web3()
            .eth()
            .transaction_count(self.account.address(), None)
            .await
            .context("transaction_count")
    }

    /// Keep polling the account's nonce until it is different from initial_nonce returning the new
    /// nonce.
    async fn wait_for_nonce_to_change(&self, initial_nonce: U256) -> U256 {
        const POLL_INTERVAL: Duration = Duration::from_secs(1);
        loop {
            match self.nonce().await {
                Ok(nonce) if nonce != initial_nonce => return nonce,
                Ok(_) => (),
                Err(err) => tracing::error!("web3 error while getting nonce: {:?}", err),
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Keep submitting the settlement transaction to the network as gas price changes.
    ///
    /// Returns when simulation of the transaction fails. This likely happens if the settlement
    /// becomes invalid due to changing prices or the account's nonce changes.
    ///
    /// Potential transaction hashes are communicated back through a shared vector.
    async fn submit_with_increasing_gas_prices_until_simulation_fails(
        &self,
        settlement: Settlement,
        nonce: U256,
        params: &SubmitterParams,
        transactions: &mut Vec<H256>,
    ) -> SubmissionError {
        let target_confirm_time = Instant::now() + params.target_confirm_time;

        // gas price and raw signed transaction
        let mut previous_tx: Option<(EstimatedGasPrice, TransactionHandle)> = None;

        loop {
            // Account for some buffer in the gas limit in case racing state changes result in slightly more heavy computation at execution time.
            let gas_limit = params.gas_estimate.to_f64_lossy() * ESTIMATE_GAS_LIMIT_FACTOR;
            let time_limit = target_confirm_time.saturating_duration_since(Instant::now());
            let gas_price = match self
                .gas_price_estimator
                .estimate_with_limits(gas_limit, time_limit)
                .await
            {
                Ok(gas_price) => gas_price,
                Err(err) => {
                    tracing::error!("gas estimation failed: {:?}", err);
                    tokio::time::sleep(params.retry_interval).await;
                    continue;
                }
            };

            // create transaction

            let method = self.build_method(settlement.clone(), &gas_price, nonce, gas_limit);

            // simulate transaction

            if let Err(err) = method.clone().view().call().await {
                if let Some((_, previous_tx)) = previous_tx.as_ref() {
                    if let Err(err) = self.submit_api.cancel_transaction(previous_tx).await {
                        tracing::warn!("cancellation failed: {:?}", err);
                    }
                }
                return SubmissionError::from(err);
            }

            // If gas price has increased cancel old and submit new transaction.

            if let Some((previous_gas_price, previous_tx)) = previous_tx.as_ref() {
                let previous_gas_price = previous_gas_price.bump(1.125).ceil();
                if gas_price.cap() > previous_gas_price.cap()
                    && gas_price.tip() > previous_gas_price.tip()
                {
                    if let Err(err) = self.submit_api.cancel_transaction(previous_tx).await {
                        tracing::warn!("cancellation failed: {:?}", err);
                    }
                } else {
                    tokio::time::sleep(params.retry_interval).await;
                    continue;
                }
            }

            // Unwrap because no communication with the node is needed because we specified nonce and gas.
            let (raw_signed_transaction, hash) =
                match method.tx.build().now_or_never().unwrap().unwrap() {
                    Transaction::Request(_) => unreachable!("verified offline account was used"),
                    Transaction::Raw { bytes, hash } => (bytes.0, hash),
                };

            tracing::info!(
                "creating transaction with hash {:?}, gas price {:?}, gas estimate {}",
                hash,
                gas_price,
                params.gas_estimate,
            );

            // Save tx hash regardless of submission success, it's not significant overhead
            // Some apis (Eden) returns failed response for submission even if its successfull,
            // we want to catch mined txs for this case
            transactions.push(hash);

            match self
                .submit_api
                .submit_raw_transaction(&raw_signed_transaction)
                .await
            {
                Ok(id) => {
                    previous_tx = Some((gas_price, id));
                }
                Err(err) => match err {
                    SubmitApiError::InvalidNonce => {
                        tracing::warn!("submission failed: invalid nonce")
                    }
                    SubmitApiError::OpenEthereumTooCheapToReplace => {
                        tracing::debug!("submission failed because OE has different replacement rules than our algorithm")
                    }
                    SubmitApiError::Other(err) => tracing::error!("submission failed: {}", err),
                },
            }
            tokio::time::sleep(params.retry_interval).await;
        }
    }

    /// Prepare transaction for simulation
    fn build_method(
        &self,
        settlement: Settlement,
        gas_price: &EstimatedGasPrice,
        nonce: U256,
        gas_limit: f64,
    ) -> MethodBuilder<DynTransport, ()> {
        let gas_price = if let Some(eip1559) = gas_price.eip1559 {
            (eip1559.max_fee_per_gas, eip1559.max_priority_fee_per_gas).into()
        } else {
            gas_price.legacy.into()
        };

        settle_method_builder(self.contract, settlement.into(), self.account.clone())
            .nonce(nonce)
            .gas(U256::from_f64_lossy(gas_limit))
            .gas_price(gas_price)
    }
}

/// From a list of potential hashes find one that was mined.
async fn find_mined_transaction(web3: &Web3, hashes: &[H256]) -> Option<TransactionReceipt> {
    // It would be nice to use the nonce and account address to find the transaction hash but there
    // is no way to do this in ethrpc api so we have to check the candidates one by one.
    let web3 = web3::Web3::new(web3::transports::Batch::new(web3.transport()));
    let futures = hashes
        .iter()
        .map(|&hash| web3.eth().transaction_receipt(hash))
        .collect::<Vec<_>>();
    if let Err(err) = web3.transport().submit_batch().await {
        tracing::error!("mined transaction batch failed: {:?}", err);
        return None;
    }
    for future in futures {
        match future.now_or_never().unwrap() {
            Err(err) => {
                tracing::error!("mined transaction individual failed: {:?}", err);
            }
            Ok(Some(transaction)) if transaction.block_hash.is_some() => return Some(transaction),
            Ok(_) => (),
        }
    }
    None
}

#[cfg(test)]
mod tests {

    use super::super::flashbots_api::FlashbotsApi;
    use super::*;
    use ethcontract::PrivateKey;
    use gas_estimation::blocknative::BlockNative;
    use reqwest::Client;
    use shared::transport::create_env_test_transport;
    use tracing::level_filters::LevelFilter;

    #[tokio::test]
    #[ignore]
    async fn flashbots_mainnet_settlement() {
        shared::tracing::initialize(
            "solver=debug,shared=debug,shared::transport::http=info",
            LevelFilter::OFF,
        );

        let web3 = Web3::new(create_env_test_transport());
        let chain_id = web3.eth().chain_id().await.unwrap().as_u64();
        assert_eq!(chain_id, 1);
        let private_key: PrivateKey = std::env::var("PRIVATE_KEY").unwrap().parse().unwrap();
        let account = Account::Offline(private_key, Some(chain_id));
        let contract = crate::get_settlement_contract(&web3).await.unwrap();
        let flashbots_api = FlashbotsApi::new(Client::new());
        let mut header = reqwest::header::HeaderMap::new();
        header.insert(
            "AUTHORIZATION",
            reqwest::header::HeaderValue::from_str(&std::env::var("BLOCKNATIVE_API_KEY").unwrap())
                .unwrap(), //or replace with api_key
        );
        let gas_price_estimator = BlockNative::new(
            shared::gas_price_estimation::Client(reqwest::Client::new()),
            header,
        )
        .await
        .unwrap();
        let gas_price_estimator = SubmitterGasPriceEstimator {
            inner: &gas_price_estimator,
            additional_tip: Some(3.0),
            gas_price_cap: 100e9,
        };

        let settlement = Settlement::new(Default::default());
        let gas_estimate =
            crate::settlement_simulation::simulate_and_estimate_gas_at_current_block(
                std::iter::once((account.clone(), settlement.clone())),
                &contract,
                &web3,
                Default::default(),
            )
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .unwrap();

        let submitter =
            Submitter::new(&contract, &account, &flashbots_api, &gas_price_estimator).unwrap();

        let params = SubmitterParams {
            target_confirm_time: Duration::from_secs(0),
            gas_estimate,
            deadline: Some(Instant::now() + Duration::from_secs(90)),
            retry_interval: Duration::from_secs(5),
        };
        let result = submitter.submit(settlement, params).await;
        tracing::info!("finished with result {:?}", result);
    }
}
