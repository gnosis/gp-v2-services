use crate::price_estimation::{Estimate, PriceEstimating, PriceEstimationError, Query};
use anyhow::Result;
use futures::future::Shared;
use futures::FutureExt;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

type EstimationResult = Result<Estimate, PriceEstimationError>;
type SharedEstimationRequest = Shared<Pin<Box<dyn Future<Output = EstimationResult> + Send>>>;

struct Inner {
    estimator: Box<dyn PriceEstimating>,
    in_flight_requests: Mutex<HashMap<Query, SharedEstimationRequest>>,
}

impl Inner {
    fn collect_garbage(&self) {
        // TODO: Refactor this to use `HashMap::drain_filter` when it's stable:
        // https://github.com/rust-lang/rust/issues/59618
        let mut active_requests = self.in_flight_requests.lock().unwrap();
        let completed_and_ignored_requests: Vec<_> = active_requests
            .iter()
            .filter_map(|(query, handle)| {
                if matches!(handle.strong_count(), Some(1)) {
                    // Only `Inner::active_requests` is still holding on to it.
                    Some(*query)
                } else {
                    None
                }
            })
            .collect();
        for query in &completed_and_ignored_requests {
            active_requests.remove(query);
        }
    }
}

/// A price estimator which doesn't issue another estimation request while an identical one is
/// already in-flight.
pub struct BufferingPriceEstimator {
    inner: Arc<Inner>,
}

impl BufferingPriceEstimator {
    pub fn new(estimator: Box<dyn PriceEstimating>) -> Self {
        Self {
            inner: Arc::new(Inner {
                estimator,
                in_flight_requests: Mutex::new(Default::default()),
            }),
        }
    }

    async fn estimate_buffered(&self, queries: &[Query]) -> Vec<EstimationResult> {
        // For each `Query` either get an in-flight request or keep the `Query` to forward it to the
        // inner price estimator.
        let (active_requests, remaining_queries): (Vec<_>, Vec<_>) = {
            let requests = self.inner.in_flight_requests.lock().unwrap();
            queries
                .iter()
                .map(|query| match requests.get(query).cloned() {
                    Some(active_request) => (Some(active_request), None),
                    None => (None, Some(*query)),
                })
                .unzip()
        };

        // Create future which estimates all `remaining_queries` in a single batch.
        let fetch_remaining_estimates = {
            let remaining_queries: Vec<_> = remaining_queries.iter().flatten().cloned().collect();
            let inner = self.inner.clone();
            async move { inner.estimator.estimates(&remaining_queries).await }
                .boxed()
                .shared()
        };

        // Create a `SharedEstimationRequest` for each individual `Query` of the batch. This
        // makes it possible for a `batch_2` to await the queries which it is interested in of the
        // in-flight `batch_1`. Even if the estimator which requested `batch_1` stops polling it, the
        // estimator of `batch_2` can still poll `batch_1` to completion by polling the
        // `SharedEstimationRequest` it is actually interested in.
        //
        // Build those shared futures up front to keep the critical section short when inserting
        // them into the `active_requests`.
        #[allow(clippy::needless_collect)]
        let individual_requests_for_batch: Vec<_> = remaining_queries
            .iter()
            .flatten()
            .enumerate()
            .map(|(index, query)| {
                let fetch_remaining_estimates = fetch_remaining_estimates.clone();
                (
                    *query,
                    async move { fetch_remaining_estimates.await[index].clone() }
                        .boxed()
                        .shared(),
                )
            })
            .collect();
        self.inner
            .in_flight_requests
            .lock()
            .unwrap()
            // It is possible that someone stored a `SharedEstimationRequest` in the meantime which
            // could be overwritten now. In those rare cases 2 identical estimation requests would
            // be in-flight at the same time but it wouldn't produce errors. No memory would leak
            // and a calling estimator could still decide to stop polling a future while other
            // estimators interested in the result are able poll the future to completion.
            .extend(individual_requests_for_batch.into_iter());

        // Await all the estimates we need (in-flight and the new ones) in parallel.
        let results = futures::join!(
            futures::future::join_all(
                active_requests
                    .iter()
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
            fetch_remaining_estimates
        );
        let (mut in_flight_results, mut new_results) =
            (results.0.into_iter(), results.1.into_iter());

        // Return the results of new and in-flight requests merged into one.
        active_requests
            .iter()
            .map(|request| match request {
                Some(_) => in_flight_results.next().unwrap(),
                None => new_results.next().unwrap(),
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl PriceEstimating for BufferingPriceEstimator {
    async fn estimates(&self, queries: &[Query]) -> Vec<EstimationResult> {
        let result = self.estimate_buffered(queries).await;
        self.inner.collect_garbage();
        result
    }
}
