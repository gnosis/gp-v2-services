use super::submitter::{SubmitApiError, TransactionHandle, TransactionSubmitting};
use anyhow::{anyhow, Result};
use futures::FutureExt;
use jsonrpc_core::Output;
use serde::de::DeserializeOwned;
use shared::Web3;

pub fn parse_json_rpc_response<T>(body: &str) -> Result<T, SubmitApiError>
where
    T: DeserializeOwned,
{
    match serde_json::from_str::<Output>(body) {
        Ok(output) => match output {
            Output::Success(body) => serde_json::from_value::<T>(body.result).map_err(|_| {
                SubmitApiError::Other(anyhow!(
                    "failed conversion to expected type {}",
                    std::any::type_name::<T>()
                ))
            }),
            Output::Failure(body) => {
                if body.error.message.contains("invalid nonce") {
                    Err(SubmitApiError::InvalidNonce)
                } else if body
                    .error
                    .message
                    .contains("Transaction gas price supplied is too low")
                {
                    Err(SubmitApiError::OpenEthereumTooCheapToReplace)
                } else {
                    Err(SubmitApiError::Other(anyhow!("rpc error: {}", body.error)))
                }
            }
        },
        Err(_) => {
            tracing::info!("invalid rpc response: {}", body);
            Err(SubmitApiError::Other(anyhow!("invalid rpc response")))
        }
    }
}

#[derive(Clone)]
pub struct CustomNodesApi {
    nodes: Vec<Web3>,
}

impl CustomNodesApi {
    pub fn new(nodes: Vec<Web3>) -> Self {
        Self { nodes }
    }
}

#[async_trait::async_trait]
impl TransactionSubmitting for CustomNodesApi {
    async fn submit_raw_transaction(
        &self,
        raw_signed_transaction: &[u8],
    ) -> Result<TransactionHandle, SubmitApiError> {
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
                    tracing::debug!("error {}", err);
                    return Err(SubmitApiError::Other(
                        anyhow::Error::from(err).context("all nodes tx failed"),
                    ));
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
