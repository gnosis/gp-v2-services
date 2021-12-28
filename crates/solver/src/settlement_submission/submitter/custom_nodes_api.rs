use super::super::submitter::{SubmitApiError, TransactionHandle, TransactionSubmitting};
use anyhow::Result;
use ethcontract::{
    dyns::DynTransport,
    transaction::{Transaction, TransactionBuilder},
};
use futures::FutureExt;
use shared::Web3;

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
    async fn submit_transaction(
        &self,
        tx: TransactionBuilder<DynTransport>,
    ) -> Result<TransactionHandle, SubmitApiError> {
        let transaction_request = tx.build().now_or_never().unwrap().unwrap();
        let mut futures = self
            .nodes
            .iter()
            .map(|node| {
                async {
                    match transaction_request.clone() {
                        Transaction::Request(tx) => node.eth().send_transaction(tx).await,
                        Transaction::Raw { bytes, hash: _ } => {
                            node.eth().send_raw_transaction(bytes.0.into()).await
                        }
                    }
                }
                .boxed()
            })
            .collect::<Vec<_>>();

        loop {
            let (result, _index, rest) = futures::future::select_all(futures).await;
            match result {
                Ok(tx_hash) => {
                    return Ok(TransactionHandle {
                        tx_hash,
                        handle: tx_hash,
                    })
                }
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
        Ok(())
    }
}
