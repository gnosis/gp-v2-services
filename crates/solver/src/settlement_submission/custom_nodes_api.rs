use super::submitter::{TransactionHandle, TransactionSubmitting};
use anyhow::{anyhow, bail, Context, Result};
use futures::FutureExt;
use jsonrpc_core::Output;
use reqwest::Client;
use serde::de::DeserializeOwned;
use shared::Web3;

pub fn parse_json_rpc_response<T>(body: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str::<Output>(body)
        .with_context(|| {
            tracing::info!("flashbot response: {}", body);
            anyhow!("invalid flashbots response")
        })
        .and_then(|output| match output {
            Output::Success(body) => serde_json::from_value::<T>(body.result).with_context(|| {
                format!(
                    "flashbots failed conversion to expected {}",
                    std::any::type_name::<T>()
                )
            }),
            Output::Failure(body) => bail!("flashbots rpc error: {}", body.error),
        })
}

#[derive(Clone)]
pub struct CustomNodesApi {
    client: Client,
    nodes: Vec<Web3>,
}

impl CustomNodesApi {
    pub fn new(client: Client, nodes: Vec<Web3>) -> Self {
        Self { client, nodes }
    }
}

#[async_trait::async_trait]
impl TransactionSubmitting for CustomNodesApi {
    async fn submit_raw_transaction(
        &self,
        raw_signed_transaction: &[u8],
    ) -> Result<TransactionHandle> {
        let mut futures = self
            .nodes
            .iter()
            .map(|node| {
                async {
                    node.eth()
                        .send_raw_transaction(raw_signed_transaction.into())
                        .await
                }
                .boxed()
            })
            .collect::<Vec<_>>();

        loop {
            let (result, _index, rest) = futures::future::select_all(futures).await;
            match result {
                Ok(hash) => return Ok(TransactionHandle(hash)),
                Err(err) if rest.is_empty() => {
                    return Err(anyhow::Error::from(err).context("all nodes tx failed"))
                }
                Err(err) => {
                    tracing::warn!(?err, "single node tx failed");
                    futures = rest;
                }
            }
        }
    }

    async fn cancel_transaction(&self, _id: &TransactionHandle) -> Result<()> {
        Ok(()) //todo ds
    }
}
