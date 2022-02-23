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
        let (active_requests, fetch_remaining_estimates) = {
            let mut in_flight_requests = self.inner.in_flight_requests.lock().unwrap();

            // For each `Query` either get an in-flight request or keep the `Query` to forward it to the
            // inner price estimator.
            let (active_requests, remaining_queries): (Vec<_>, Vec<_>) = queries
                .iter()
                .map(|query| match in_flight_requests.get(query).cloned() {
                    Some(active_request) => (Some(active_request), None),
                    None => (None, Some(*query)),
                })
                .unzip();

            // Create future which estimates all `remaining_queries` in a single batch.
            let fetch_remaining_estimates = {
                let remaining_queries: Vec<_> =
                    remaining_queries.iter().flatten().cloned().collect();
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
            in_flight_requests.extend(remaining_queries.iter().flatten().enumerate().map(
                |(index, query)| {
                    let fetch_remaining_estimates = fetch_remaining_estimates.clone();
                    (
                        *query,
                        async move { fetch_remaining_estimates.await[index].clone() }
                            .boxed()
                            .shared(),
                    )
                },
            ));
            (active_requests, fetch_remaining_estimates)
        };

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::price_estimation::MockPriceEstimating;
    use futures::poll;
    use maplit::hashset;
    use primitive_types::H160;
    use std::collections::HashSet;
    use std::time::Duration;
    use tokio::time::sleep;

    fn in_flight_requests(buffered: &BufferingPriceEstimator) -> HashSet<Query> {
        HashSet::from_iter(
            buffered
                .inner
                .in_flight_requests
                .lock()
                .unwrap()
                .keys()
                .cloned(),
        )
    }

    #[tokio::test]
    async fn request_can_be_completed_by_request_depending_on_it() {
        let estimate = |amount: u64| Estimate {
            out_amount: amount.into(),
            ..Default::default()
        };
        let query = |address| Query {
            sell_token: H160::from_low_u64_be(address),
            ..Default::default()
        };

        let first_batch = [query(1), query(2)];
        let second_batch = [query(2), query(3)];

        let mut estimator = Box::new(MockPriceEstimating::new());
        estimator
            .expect_estimates()
            .times(1)
            .returning(move |queries| {
                assert_eq!(queries, first_batch);
                let result = vec![Ok(estimate(1)), Ok(estimate(2))];
                async move {
                    sleep(Duration::from_millis(10)).await;
                    result
                }
                .boxed()
            });

        estimator
            .expect_estimates()
            .times(1)
            .returning(move |queries| {
                // only the missing query actually needs to be estimated
                assert_eq!(queries, &vec![query(3)]);
                let result = vec![Ok(estimate(3))];
                async move {
                    sleep(Duration::from_millis(10)).await;
                    result
                }
                .boxed()
            });

        let buffered = BufferingPriceEstimator::new(estimator);
        let first_batch_request = buffered.estimates(&first_batch).shared();
        let second_batch_request = buffered.estimates(&second_batch).shared();

        assert!(buffered.inner.in_flight_requests.lock().unwrap().is_empty());

        // Poll first batch to store futures for its inidividual queries.
        let _ = poll!(first_batch_request.clone());
        assert_eq!(
            in_flight_requests(&buffered),
            hashset! { query(1), query(2) }
        );

        // Poll second batch to store futures for its NEW inidividual queries.
        let _ = poll!(second_batch_request.clone());
        assert_eq!(
            in_flight_requests(&buffered),
            hashset! { query(1), query(2), query(3) }
        );

        drop(first_batch_request);
        // Drop all futures which nobody depends on anymore.
        buffered.inner.collect_garbage();
        assert_eq!(in_flight_requests(&buffered), hashset! { query(2) });

        // Poll second future to completion.
        let second_batch_result = second_batch_request.await;
        assert_eq!(second_batch_result.len(), 2);
        // Although the initiator of the request for `query(2)` dropped its future, other futures
        // depending on the result can still drive the original future to completion.
        assert_eq!(second_batch_result[0].as_ref().unwrap(), &estimate(2));
        assert_eq!(second_batch_result[1].as_ref().unwrap(), &estimate(3));

        // Polling the future to completion also collects garbage at the end.
        assert!(buffered.inner.in_flight_requests.lock().unwrap().is_empty());
    }
}
