pub mod optimize_unwrapping;

use crate::settlement::Settlement;
use crate::settlement_simulation::simulate_and_estimate_gas_at_current_block;
use crate::solver::http_solver::buffers::BufferRetriever;
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
    buffer_retriever: BufferRetriever,
}

impl PostProcessingPipeline {
    pub fn new(
        native_token: H160,
        web3: Web3,
        unwrap_factor: f64,
        settlement_contract: GPv2Settlement,
    ) -> Self {
        let weth = WETH9::at(&web3, native_token);
        let buffer_retriever = BufferRetriever::new(web3.clone(), settlement_contract.address());

        Self {
            web3,
            unwrap_factor,
            settlement_contract,
            weth,
            buffer_retriever,
        }
    }

    pub async fn optimize_settlement(
        &self,
        settlement: Settlement,
        solver_account: Account,
        gas_price: EstimatedGasPrice,
    ) -> Settlement {
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

        // an error will leave the settlement unmodified
        optimize_unwrapping(
            settlement,
            &settlement_would_succeed,
            &self.buffer_retriever,
            &self.weth,
            self.unwrap_factor,
        )
        .await
    }
}
