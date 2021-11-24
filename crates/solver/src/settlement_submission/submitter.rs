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

use super::ESTIMATE_GAS_LIMIT_FACTOR;
use crate::{interactions::block_coinbase, settlement::Settlement};
use anyhow::{anyhow, ensure, Context, Error, Result};
use contracts::GPv2Settlement;
use ethcontract::{
    contract::MethodBuilder, dyns::DynTransport, transaction::Transaction,
    Account,
};
use futures::FutureExt;
use gas_estimation::{EstimatedGasPrice, GasPriceEstimating};
use primitive_types::{H256, U256};
use shared::Web3;
use std::time::{Duration, Instant, SystemTime};
use web3::types::TransactionReceipt;

/// Parameters for transaction submitting. todo
#[derive(Clone, Default)]
pub struct SubmitterParams {
    /// Desired duration to include the transaction in a block
    pub target_confirm_time: Duration, //todo change to blocks
    /// Estimated gas consumption of a transaction
    pub gas_estimate: U256,
    /// Maximum max_fee_per_gas to pay for a transaction
    pub gas_price_cap: f64,
    /// Maximum duration of a single run loop
    pub deadline: Option<SystemTime>,
    /// There are two ways to pay for a transaction:
    /// 1. Set valid gas price (default)
    /// 2. Set 0 gas price, but add additional step to settlement to pay directly to the coinbase
    /// Contains quantity of gas needed to pay for the additional step
    pub pay_gas_to_coinbase: Option<U256>,
    /// Boost estimated gas price miner tip in order to increase the chances of a transaction being mined
    pub additional_miner_tip: Option<f64>,
    /// Resimulate and resend transaction on every update_interval seconds
    pub update_interval: Duration,
}

#[async_trait::async_trait]
pub trait TransactionSubmitting {
    async fn submit_raw_transaction(&self, tx: &[u8], params: &SubmitterParams) -> Result<String>;
    async fn cancel_transaction(&self, id: &str) -> Result<()>;
}

pub struct Submitter<'a> {
    web3: &'a Web3,
    contract: &'a GPv2Settlement,
    account: &'a Account,
    submit_api: &'a dyn TransactionSubmitting,
    gas_price_estimator: &'a dyn GasPriceEstimating,
}

impl<'a> Submitter<'a> {
    pub fn new(
        web3: &'a Web3,
        contract: &'a GPv2Settlement,
        account: &'a Account,
        submit_api: &'a dyn TransactionSubmitting,
        gas_price_estimator: &'a dyn GasPriceEstimating,
    ) -> Result<Self> {
        ensure!(
            matches!(account, Account::Offline(..)),
            "Submission requires offline account for signing"
        );

        Ok(Self {
            web3,
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
    ) -> Result<Option<TransactionReceipt>> {
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
            Some(deadline) => deadline
                .duration_since(SystemTime::now())
                .unwrap_or_else(|_| Duration::from_secs(0)),
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
                if let Some(receipt) = find_mined_transaction(self.web3, &transactions).await {
                    tracing::info!("found mined transaction {}", receipt.transaction_hash);
                    return Ok(Some(receipt));
                }
                if Instant::now() + MINED_TX_CHECK_INTERVAL > tx_to_propagate_deadline {
                    break;
                }
                tokio::time::sleep(MINED_TX_CHECK_INTERVAL).await;
            }
        }

        tracing::info!("did not find any mined transaction");
        fallback_result
    }

    async fn nonce(&self) -> Result<U256> {
        self.web3
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

    async fn gas_price(
        &self,
        gas_limit: f64,
        time_limit: Duration,
        params: &SubmitterParams,
    ) -> Result<EstimatedGasPrice> {
        match self
            .gas_price_estimator
            .estimate_with_limits(gas_limit, time_limit)
            .await
        {
            Ok(mut gas_price) if gas_price.cap() <= params.gas_price_cap => {
                // boost miner tip to increase our chances of being included in a block
                if let Some(ref mut eip1559) = gas_price.eip1559 {
                    eip1559.max_priority_fee_per_gas +=
                        params.additional_miner_tip.unwrap_or_default();
                }
                Ok(gas_price)
            }
            Ok(gas_price) => Err(anyhow!(
                "gas station gas price {} is larger than cap {}",
                gas_price.cap(),
                params.gas_price_cap
            )),
            Err(err) => Err(err),
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
    ) -> anyhow::Error {
        let target_confirm_time = Instant::now() + params.target_confirm_time;

        // gas price and raw signed transaction
        let mut previous_tx: Option<(EstimatedGasPrice, String)> = None;

        loop {
            // Account for some buffer in the gas limit in case racing state changes result in slightly more heavy computation at execution time.
            let gas_limit = gas_estimate(params).to_f64_lossy() * ESTIMATE_GAS_LIMIT_FACTOR;
            let time_limit = target_confirm_time.saturating_duration_since(Instant::now());
            let gas_price = match self.gas_price(gas_limit, time_limit, params).await {
                Ok(gas_price) => gas_price,
                Err(err) => {
                    tracing::error!("gas estimation failed: {:?}", err);
                    tokio::time::sleep(params.update_interval).await;
                    continue;
                }
            };

            // create transaction

            let method =
                self.build_method(settlement.clone(), params, &gas_price, nonce, gas_limit);

            // simulate transaction

            if let Err(err) = method.clone().view().call().await {
                if let Some((_, previous_tx)) = previous_tx.as_ref() {
                    if let Err(err) = self.submit_api.cancel_transaction(previous_tx).await {
                        tracing::warn!("cancellation failed: {:?}", err);
                    }
                }
                return Error::from(err).context("failed simulation");
            }

            // If gas price has increased cancel old and submit new new transaction.

            if let Some((previous_gas_price, previous_tx)) = previous_tx.as_ref() {
                let previous_gas_price = previous_gas_price.bump(1.125).ceil();
                if gas_price.cap() > previous_gas_price.cap()
                    && gas_price.tip() > previous_gas_price.tip()
                {
                    if let Err(err) = self.submit_api.cancel_transaction(previous_tx).await {
                        tracing::warn!("cancellation failed: {:?}", err);
                    }
                } else {
                    tokio::time::sleep(params.update_interval).await;
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
                "creating transaction with hash {:?}, tip to miner {:.3e}, gas price {:?}, gas estimate {}",
                hash,
                tx_gas_cost(&gas_price, params).to_f64_lossy(),
                gas_price,
                gas_estimate(params),
            );

            match self
                .submit_api
                .submit_raw_transaction(&raw_signed_transaction, params)
                .await
            {
                Ok(id) => {
                    transactions.push(hash);
                    previous_tx = Some((gas_price, id));
                }
                Err(err) => tracing::error!("submission failed: {:?}", err),
            }
            tokio::time::sleep(params.update_interval).await;
        }
    }

    /// Prepare transaction for simulation
    fn build_method(
        &self,
        mut settlement: Settlement,
        params: &SubmitterParams,
        gas_price: &EstimatedGasPrice,
        nonce: U256,
        gas_limit: f64,
    ) -> MethodBuilder<DynTransport, ()> {
        // for some private networks we need to pay directly to coinbase for the transaction
        let (gas_price, settlement) = match params.pay_gas_to_coinbase {
            Some(_) => {
                // gas price iz zero, but we add additional step to settlement to pay to coinbase
                settlement
                    .encoder
                    .append_to_execution_plan(block_coinbase::PayBlockCoinbase {
                        amount: tx_gas_cost(gas_price, params),
                    });
                ((0.0, 0.0).into(), settlement) //todo check if archer supports 1559?
            }
            None => {
                // define gas price and use settlement as is
                let gas_price = if let Some(eip1559) = gas_price.eip1559 {
                    (eip1559.max_fee_per_gas, eip1559.max_priority_fee_per_gas).into()
                } else {
                    gas_price.legacy.into()
                };
                (gas_price, settlement)
            }
        };

        super::retry::settle_method_builder(self.contract, settlement.into(), self.account.clone())
            .nonce(nonce)
            .gas(U256::from_f64_lossy(gas_limit))
            .gas_price(gas_price)
    }
}

fn gas_estimate(params: &SubmitterParams) -> U256 {
    params.gas_estimate + params.pay_gas_to_coinbase.unwrap_or_default()
}

fn tx_gas_cost(gas_price: &EstimatedGasPrice, params: &SubmitterParams) -> U256 {
    U256::from_f64_lossy(gas_price.effective_gas_price()) * gas_estimate(params)
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

    use super::*;
    use ethcontract::PrivateKey;
    use gas_estimation::blocknative::BlockNative;
    use reqwest::Client;
    use shared::transport::create_env_test_transport;
    use tracing::level_filters::LevelFilter;

    #[tokio::test]
    #[ignore]
    async fn mainnet_settlement() {
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
        let gas_price_cap = 100e9;

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

        let submitter = FlashbotsSolutionSubmitter::new(
            &web3,
            &contract,
            &account,
            &flashbots_api,
            &gas_price_estimator,
            gas_price_cap,
        )
        .unwrap();

        let result = submitter
            .submit(Duration::from_secs(0), settlement, gas_estimate)
            .await;
        tracing::info!("finished with result {:?}", result);
    }
}
