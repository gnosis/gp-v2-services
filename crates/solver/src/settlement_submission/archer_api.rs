//! https://docs.archerdao.io/for-traders/for-traders/traders

use super::submitter::{SubmitterParams, TransactionSubmitting};
use anyhow::{anyhow, ensure, Result};
use reqwest::Client;
use std::time::Instant;

const URL: &str = "https://api.archerdao.io/v1/transaction";

#[derive(Clone)]
pub struct ArcherApi {
    client: Client,
    authorization: String,
}

impl ArcherApi {
    pub fn new(authorization: String, client: Client) -> Self {
        Self {
            client,
            authorization,
        }
    }
}

#[async_trait::async_trait]
impl TransactionSubmitting for ArcherApi {
    async fn submit_raw_transaction(
        &self,
        raw_signed_transaction: &[u8],
        params: &SubmitterParams,
    ) -> Result<String> {
        let id = format!("0x{}", hex::encode(raw_signed_transaction));
        let deadline = params
            .deadline
            .ok_or_else(|| anyhow!("deadline not defined"))?
            .saturating_duration_since(Instant::now())
            .as_secs()
            .to_string();
        let body = serde_json::json!({
          "jsonrpc": "2.0",
          "id": 1,
          "method": "archer_submitTx",
          "tx": id,
          "deadline": deadline,
        });
        tracing::debug!(
            "archer submit_transaction body: {}",
            serde_json::to_string(&body).unwrap_or_else(|err| format!("error: {:?}", err)),
        );
        let response = self
            .client
            .post(URL)
            .header("Authorization", &self.authorization)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        ensure!(status.is_success(), "status {}: {:?}", status, body);
        Ok(id)
    }

    async fn cancel_transaction(&self, id: &str) -> Result<()> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "archer_cancelTx",
            "tx": id,
        });
        tracing::debug!(
            "archer_cancelTx body: {}",
            serde_json::to_string(&body).unwrap_or_else(|err| format!("error: {:?}", err)),
        );
        let response = self
            .client
            .post(URL)
            .header("Authorization", &self.authorization)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        ensure!(status.is_success(), "status {}: {:?}", status, body);
        Ok(())
    }
}
