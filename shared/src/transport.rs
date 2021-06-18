use crate::http_transport::HttpTransport;
use derivative::Derivative;
use ethcontract::jsonrpc::types::{Call, Value};
use ethcontract::web3::{error, BatchTransport, RequestId, Transport};
use futures::future::BoxFuture;
use futures::FutureExt;
use std::{
    convert::TryInto,
    sync::Arc,
    time::{Duration, Instant},
};

/// Convenience method to create our standard instrumented transport
pub fn create_instrumented_transport<T>(
    transport: T,
    metrics: Arc<dyn TransportMetrics>,
) -> MetricTransport<T>
where
    T: Transport,
    <T as Transport>::Out: Send + 'static,
{
    MetricTransport::new(transport, metrics)
}

/// Convenience method to create a compatible transport without metrics (noop)
pub fn create_test_transport(url: &str) -> MetricTransport<HttpTransport>
where
{
    let transport = HttpTransport::new(url.try_into().unwrap());
    MetricTransport::new(transport, Arc::new(NoopTransportMetrics))
}

/// Like above but takes url from the environment NODE_URL.
pub fn create_env_test_transport() -> MetricTransport<HttpTransport>
where
{
    let env = std::env::var("NODE_URL").unwrap();
    let transport = HttpTransport::new(env.parse().unwrap());
    MetricTransport::new(transport, Arc::new(NoopTransportMetrics))
}

pub trait TransportMetrics: Send + Sync {
    fn report_query(&self, label: &str, elapsed: Duration);
}
#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub struct MetricTransport<T: Transport> {
    inner: T,
    #[derivative(Debug = "ignore")]
    metrics: Arc<dyn TransportMetrics>,
}

impl<T: Transport> MetricTransport<T> {
    pub fn new(inner: T, metrics: Arc<dyn TransportMetrics>) -> MetricTransport<T> {
        Self { inner, metrics }
    }
}

impl<T> Transport for MetricTransport<T>
where
    T: Transport,
    <T as Transport>::Out: Send + 'static,
{
    type Out = BoxFuture<'static, error::Result<Value>>;

    fn prepare(&self, method: &str, params: Vec<Value>) -> (RequestId, Call) {
        self.inner.prepare(method, params)
    }

    fn send(&self, id: RequestId, request: Call) -> Self::Out {
        let metrics = self.metrics.clone();
        let start = Instant::now();
        self.inner
            .send(id, request.clone())
            .inspect(move |_| {
                let label = match request {
                    Call::MethodCall(method) => method.method,
                    Call::Notification(notification) => notification.method,
                    Call::Invalid { .. } => "invalid".into(),
                };
                metrics.report_query(&label, start.elapsed());
            })
            .boxed()
    }
}

impl<T> BatchTransport for MetricTransport<T>
where
    T: BatchTransport,
    T::Batch: Send + 'static,
    <T as Transport>::Out: Send + 'static,
{
    type Batch = BoxFuture<'static, error::Result<Vec<error::Result<Value>>>>;

    fn send_batch<I>(&self, requests: I) -> Self::Batch
    where
        I: IntoIterator<Item = (RequestId, Call)>,
    {
        let metrics = self.metrics.clone();
        let start = Instant::now();
        self.inner
            .send_batch(requests)
            .inspect(move |_| metrics.report_query(&"batch", start.elapsed()))
            .boxed()
    }
}

struct NoopTransportMetrics;
impl TransportMetrics for NoopTransportMetrics {
    fn report_query(&self, _: &str, _: Duration) {}
}
