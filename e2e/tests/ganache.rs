use ethcontract::futures::FutureExt;
use ethcontract::{Http, U256};
use shared::{transport::LoggingTransport, Web3};
use std::{
    fmt::Debug,
    future::Future,
    panic::{self, AssertUnwindSafe},
};
use web3::{api::Namespace, helpers::CallFuture, Transport};

const NODE_HOST: &str = "http://127.0.0.1:8545";

/// *Testing* function that takes a closure and executes it on Ganache.
/// Before each test, it creates a snapshot of the current state of the chain.
/// The saved state is restored at the end of the test.
///
/// This function must not be called again until the current execution has
/// terminated.
pub async fn test<F, Fut>(f: F)
where
    F: FnOnce(Web3) -> Fut,
    Fut: Future<Output = ()>,
{
    let http = LoggingTransport::new(Http::new(NODE_HOST).expect("transport failure"));
    let web3 = Web3::new(http);
    let resetter = Resetter::new(&web3).await;

    // Hack: the closure may actually be unwind unsafe; moreover, `catch_unwind`
    // does not catch some types of panics. In this cases, the state of the node
    // is not restored. This is not considered an issue since this function
    // is supposed to be used in a test environment.
    let result = AssertUnwindSafe(f(web3.clone())).catch_unwind().await;

    resetter.reset().await;

    if let Err(err) = result {
        panic::resume_unwind(err);
    }
}

struct Resetter<T> {
    ganache: GanacheApi<T>,
    snapshot_id: U256,
}

impl<T: Transport> Resetter<T> {
    async fn new(web3: &web3::Web3<T>) -> Self {
        let ganache = web3.api::<GanacheApi<_>>();
        let snapshot_id = ganache
            .snapshot()
            .await
            .expect("Test network must support evm_snapshot");
        Self {
            ganache,
            snapshot_id,
        }
    }

    async fn reset(&self) {
        self.ganache
            .revert(&self.snapshot_id)
            .await
            .expect("Test network must support evm_revert");
    }
}

#[derive(Debug, Clone)]
pub struct GanacheApi<T> {
    transport: T,
}

impl<T: Transport> Namespace<T> for GanacheApi<T> {
    fn new(transport: T) -> Self
    where
        Self: Sized,
    {
        GanacheApi { transport }
    }

    fn transport(&self) -> &T {
        &self.transport
    }
}

impl<T: Transport> GanacheApi<T> {
    pub fn snapshot(&self) -> CallFuture<U256, T::Out> {
        CallFuture::new(self.transport.execute("evm_snapshot", vec![]))
    }

    pub fn revert(&self, snapshot_id: &U256) -> CallFuture<bool, T::Out> {
        let value_id = serde_json::json!(snapshot_id);
        CallFuture::new(self.transport.execute("evm_revert", vec![value_id]))
    }
}
