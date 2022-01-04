//! A buffered `Transport` implementation that automatically groups JSON RPC
//! requests into batches.

use super::MAX_BATCH_SIZE;
use ethcontract::{
    jsonrpc::Call,
    web3::{BatchTransport, Error as Web3Error, RequestId, Transport},
};
use futures::{
    channel::{mpsc, oneshot},
    future::{BoxFuture, FutureExt as _, Shared},
    stream::{self, FusedStream, Stream, StreamExt as _},
};
use serde_json::Value;
use std::{
    fmt::{self, Debug, Formatter},
    future::Future,
    sync::Arc,
    time::Duration,
};

/// Buffered transport configuration.
pub struct Configuration {
    /// The maximum amount of concurrent batches to send to the node.
    pub max_concurrent_requests: usize,
    /// The maximum batch size.
    pub max_batch_len: usize,
    /// An additional minimum delay to wait for collecting requests.
    ///
    /// The delay starts counting after receiving the first request.
    pub batch_delay: Duration,
}

impl Default for Configuration {
    fn default() -> Self {
        // Default configuration behaves kind of like TCP Nagle.
        Self {
            max_concurrent_requests: 1,
            max_batch_len: MAX_BATCH_SIZE,
            batch_delay: Duration::default(),
        }
    }
}

/// Buffered `Transport` implementation that implements automatic batching of
/// JSONRPC requests.
#[derive(Clone)]
pub struct Buffered<Inner> {
    inner: Arc<Inner>,
    calls: mpsc::UnboundedSender<CallContext>,
    worker: Worker,
}

type RpcResult = Result<Value, Web3Error>;

type CallContext = (RequestId, Call, oneshot::Sender<RpcResult>);

type Worker = Shared<BoxFuture<'static, ()>>;

impl<Inner> Buffered<Inner>
where
    Inner: BatchTransport + Send + Sync + 'static,
    Inner::Out: Send,
    Inner::Batch: Send,
{
    /// Create a new buffered transport with the default configuration.
    pub fn new(inner: Inner) -> Self {
        Self::with_config(inner, Default::default())
    }

    /// Creates a new buffered transport with the specified configuration.
    pub fn with_config(inner: Inner, config: Configuration) -> Self {
        let inner = Arc::new(inner);
        let (calls, receiver) = mpsc::unbounded();
        let worker = Self::worker(inner.clone(), config, receiver);

        Self {
            inner,
            calls,
            worker,
        }
    }

    /// Start a background worker for handling batched requests.
    fn worker(
        inner: Arc<Inner>,
        config: Configuration,
        calls: mpsc::UnboundedReceiver<CallContext>,
    ) -> Worker {
        batched_for_each(config, calls, move |mut batch| {
            let inner = inner.clone();
            async move {
                match batch.len() {
                    0 => (),
                    1 => {
                        let (id, request, sender) = batch.remove(0);
                        let result = inner.send(id, request).await;
                        let _ = sender.send(result);
                    }
                    n => {
                        let (requests, senders): (Vec<_>, Vec<_>) = batch
                            .into_iter()
                            .map(|(id, request, sender)| ((id, request), sender))
                            .unzip();
                        let results = inner
                            .send_batch(requests)
                            .await
                            .unwrap_or_else(|err| vec![Err(err); n]);
                        for (sender, result) in senders.into_iter().zip(results) {
                            let _ = sender.send(result);
                        }
                    }
                }
            }
        })
        .boxed()
        .shared()
    }

    /// Queue a call by sending it over calls channel to the background worker.
    fn queue_call(&self, id: RequestId, request: Call) -> oneshot::Receiver<RpcResult> {
        let (sender, receiver) = oneshot::channel();
        let context = (id, request, sender);
        self.calls
            .unbounded_send(context)
            .expect("worker task unexpectedly dropped");
        receiver
    }

    /// Executes a task that is dependent on the background worker.
    ///
    /// This methods takes extra care to always drive the background worker
    /// future per call. This is done so `call` futures resolve even if we
    /// drop the transport.
    fn task<T, Fut>(&self, fut: Fut) -> BoxFuture<'static, T>
    where
        Fut: Future<Output = T> + Send + 'static,
    {
        let fut = fut.fuse();

        let mut worker = self.worker.clone();
        async move {
            futures::pin_mut!(fut);
            futures::select_biased! {
                result = fut => result,
                _ = worker => fut
                    .now_or_never()
                    .expect("worker task did not resolve pending calls"),
            }
        }
        .boxed()
    }
}

impl<Inner> Debug for Buffered<Inner>
where
    Inner: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Buffered")
            .field("inner", &self.inner)
            .field("calls", &self.calls)
            .finish()
    }
}

impl<Inner> Transport for Buffered<Inner>
where
    Inner: BatchTransport + Send + Sync + 'static,
    Inner::Out: Send,
    Inner::Batch: Send,
{
    type Out = BoxFuture<'static, RpcResult>;

    fn prepare(&self, method: &str, params: Vec<Value>) -> (RequestId, Call) {
        self.inner.prepare(method, params)
    }

    fn send(&self, id: RequestId, request: Call) -> Self::Out {
        let response = self.queue_call(id, request);
        self.task(async move { response.await.expect("worker task unexpectedly dropped") })
            .boxed()
    }
}

impl<Inner> BatchTransport for Buffered<Inner>
where
    Inner: BatchTransport + Send + Sync + 'static,
    Inner::Out: Send,
    Inner::Batch: Send,
{
    type Batch = BoxFuture<'static, Result<Vec<RpcResult>, Web3Error>>;

    fn send_batch<T>(&self, _requests: T) -> Self::Batch
    where
        T: IntoIterator<Item = (RequestId, Call)>,
    {
        todo!()
    }
}

/// Batches a stream into chunks.
///
/// This is very similar to `futures::future::FutureExt::ready_chunks` with the
/// difference that it allows configuring:
/// - Maximum concurrency for work done on the chunks (allowing things like only
///   processing one chunk at a time, and have the subsequent chunk already
///   start accumulating in a batch)
/// - Minimum delay for a batch, so waiting for a small amount of time to allow
///   other items to resolve to increase batch sizes.
fn batched_for_each<T, St, F, Fut>(
    config: Configuration,
    items: St,
    work: F,
) -> impl Future<Output = ()>
where
    St: Stream<Item = T> + FusedStream + Unpin,
    F: Fn(Vec<T>) -> Fut,
    Fut: Future<Output = ()>,
{
    let concurrency_limit = config.max_concurrent_requests;

    let batches = stream::unfold(items, move |mut items| async move {
        let mut chunk = vec![items.next().await?];

        let delay = delay(config.batch_delay).fuse();
        futures::pin_mut!(delay);

        while chunk.len() < config.max_batch_len {
            futures::select_biased! {
                item = items.next() => match item {
                    Some(item) => chunk.push(item),
                    None => break,
                },
                _ = delay => break,
            }
        }

        Some((chunk, items))
    });

    batches.for_each_concurrent(concurrency_limit, work)
}

async fn delay(_: Duration) {
    // TODO
    futures::future::ready(()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::mock::MockTransport;
    use ethcontract::{Web3, U256};
    use mockall::predicate;
    use serde_json::json;

    #[tokio::test]
    async fn batches_calls_when_joining() {
        let transport = MockTransport::new();
        transport
            .mock()
            .expect_execute_batch()
            .with(predicate::eq(vec![
                ("foo".to_owned(), vec![json!(true), json!("stuff")]),
                ("bar".to_owned(), vec![json!(42), json!("answer")]),
            ]))
            .returning(|_| Ok(vec![Ok(json!("hello")), Ok(json!("world"))]));

        let transport = Buffered::new(transport);

        let (foo, bar) = futures::join!(
            transport.execute("foo", vec![json!(true), json!("stuff")]),
            transport.execute("bar", vec![json!(42), json!("answer")]),
        );
        assert_eq!(foo.unwrap(), json!("hello"));
        assert_eq!(bar.unwrap(), json!("world"));
    }

    #[tokio::test]
    async fn no_batching_with_only_one_request() {
        let transport = MockTransport::new();
        transport
            .mock()
            .expect_execute()
            .with(
                predicate::eq("single".to_owned()),
                predicate::eq(vec![json!("request")]),
            )
            .returning(|_, _| Ok(json!(42)));

        let transport = Buffered::new(transport);

        let response = transport
            .execute("single", vec![json!("request")])
            .await
            .unwrap();
        assert_eq!(response, json!(42));
    }

    #[tokio::test]
    async fn batches_separate_web3_instances() {
        let transport = MockTransport::new();
        transport
            .mock()
            .expect_execute_batch()
            .with(predicate::eq(vec![
                ("eth_chainId".to_owned(), vec![]),
                ("eth_chainId".to_owned(), vec![]),
                ("eth_chainId".to_owned(), vec![]),
            ]))
            .returning(|_| {
                Ok(vec![
                    Ok(json!("0x2a")),
                    Ok(json!("0x2a")),
                    Ok(json!("0x2a")),
                ])
            });

        let web3 = Web3::new(Buffered::new(transport));

        let chain_ids = futures::future::try_join_all(vec![
            web3.clone().eth().chain_id(),
            web3.clone().eth().chain_id(),
            web3.clone().eth().chain_id(),
        ])
        .await
        .unwrap();

        assert_eq!(chain_ids, vec![U256::from(42); 3]);
    }

    #[tokio::test]
    async fn resolves_call_after_dropping_transport() {
        let transport = MockTransport::new();
        transport
            .mock()
            .expect_execute_batch()
            .with(predicate::eq(vec![
                ("unused".to_owned(), vec![]),
                ("used".to_owned(), vec![]),
            ]))
            .returning(|_| Ok(vec![Ok(json!(0)), Ok(json!(1))]));

        let transport = Buffered::new(transport);

        let unused = transport.execute("unused", vec![]);
        let used = transport.execute("used", vec![]);
        drop((unused, transport));

        assert_eq!(used.await.unwrap(), 1);
    }
}
