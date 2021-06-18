use crate::api::internal_error;
use anyhow::Result;
use serde::Deserialize;
use shared::balancer::pool_fetching::BalancerPoolFetcher;
use std::{convert::Infallible, sync::Arc};
use warp::reply::with_status;
use warp::{hyper::StatusCode, reply, Filter, Rejection, Reply};

#[derive(Clone)]
struct BalancerFilter {}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Query {}

fn get_pools_request(
) -> impl Filter<Extract = (Result<BalancerFilter, &'static str>,), Error = Rejection> + Clone {
    warp::path!("get_pools")
        .and(warp::get())
        .and(warp::query::<Query>())
        .map(|_query: Query| Ok(BalancerFilter {}))
}

pub fn get_pools(
    pool_fetcher: Arc<BalancerPoolFetcher>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    get_pools_request().and_then(move |_| {
        let fetcher = pool_fetcher.clone();
        async move {
            let result = fetcher.fetch_all().await;
            let handled_result = match result {
                Ok(pools) => reply::with_status(reply::json(&pools), StatusCode::OK),
                Err(err) => {
                    tracing::error!(?err, "get_pools error");
                    with_status(internal_error(), StatusCode::INTERNAL_SERVER_ERROR)
                }
            };
            Result::<_, Infallible>::Ok(handled_result)
        }
    })
}
