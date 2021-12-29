pub mod optimize_unwrapping;

use crate::settlement::Settlement;
use crate::settlement_simulation::simulate_and_estimate_gas_at_current_block;
use contracts::{GPv2Settlement, WETH9};
use ethcontract::Account;
use gas_estimation::EstimatedGasPrice;
use optimize_unwrapping::optimize_unwrapping;
use primitive_types::H160;
use shared::Web3;

pub struct PostProcessingPipeline {
    web3: Web3,
    unwrap_factor: f64,
    settlement_contract: GPv2Settlement,
    weth: WETH9,
}

impl PostProcessingPipeline {
    pub fn new(
        native_token: H160,
        web3: Web3,
        unwrap_factor: f64,
        settlement_contract: GPv2Settlement,
    ) -> Self {
        let weth = WETH9::at(&web3, native_token);

        Self {
            web3,
            unwrap_factor,
            settlement_contract,
            weth,
        }
    }

    pub async fn optimize_settlement(
        &self,
        settlement: &mut Settlement,
        solver_account: Account,
        gas_price: EstimatedGasPrice,
    ) {
        let settlement_would_succeed = |settlement: Settlement| async {
            let result = simulate_and_estimate_gas_at_current_block(
                std::iter::once((solver_account.clone(), settlement)),
                &self.settlement_contract,
                &self.web3,
                gas_price,
            )
            .await;
            matches!(result, Ok(results) if results[0].is_ok())
        };

        let get_weth_balance = || async {
            // TODO cache the WETH balance if we add more optimizations in the future which need it
            self.weth
                .methods()
                .balance_of(self.settlement_contract.address())
                .call()
                .await
                .map_err(|e| anyhow::anyhow!(e))
        };

        let _ = optimize_unwrapping(
            settlement,
            &settlement_would_succeed,
            &get_weth_balance,
            &self.weth,
            self.unwrap_factor,
        )
        .await;

        debug_assert!(settlement_would_succeed(settlement.clone()).await);
    }
}
