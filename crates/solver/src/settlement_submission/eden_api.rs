//! https://docs.edennetwork.io/for-traders/getting-started

use super::submitter::{SubmitApiError, TransactionHandle, TransactionSubmitting};
use anyhow::Result;
use primitive_types::H256;
use reqwest::Client;

const URL: &str = "https://api.edennetwork.io/v1/rpc";

#[derive(Clone)]
pub struct EdenApi {
    client: Client,
}

impl EdenApi {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl TransactionSubmitting for EdenApi {
    async fn submit_raw_transaction(
        &self,
        raw_signed_transaction: &[u8],
    ) -> Result<TransactionHandle, SubmitApiError> {
        let tx = format!("0x{}", hex::encode(raw_signed_transaction));
        let body = serde_json::json!({
          "jsonrpc": "2.0",
          "id": 1,
          "method": "eth_sendRawTransaction",
          "params": [tx],
        });
        tracing::debug!(
            "eden submit_transaction body: {}",
            serde_json::to_string(&body).unwrap_or_else(|err| format!("error: {:?}", err)),
        );
        let response = self
            .client
            .post(URL)
            .json(&body)
            .send()
            .await
            .map_err(|err| SubmitApiError::Other(err.into()))?;
        let body = response
            .text()
            .await
            .map_err(|err| SubmitApiError::Other(err.into()))?;

        let tx_hash = super::custom_nodes_api::parse_json_rpc_response::<H256>(&body)?;
        tracing::debug!("transaction handle: {}", tx_hash);
        Ok(TransactionHandle(tx_hash))
    }

    async fn cancel_transaction(&self, _id: &TransactionHandle) -> Result<()> {
        Ok(())
    }
}
