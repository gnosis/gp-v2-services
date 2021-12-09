mod cancel_order;
mod create_order;
mod get_fee_and_quote;
mod get_fee_info;
mod get_markets;
mod get_order_by_uid;
mod get_orders;
mod get_orders_by_tx;
mod get_solvable_orders;
mod get_solvable_orders_v2;
mod get_trades;
mod get_user_orders;
mod metrics;
pub mod order_validation;
pub mod post_quote;

use crate::{
    api::post_quote::OrderQuoter, database::trades::TradeRetrieving, orderbook::Orderbook,
};
use anyhow::{Error as anyhowError, Result};
use serde::{de::DeserializeOwned, Serialize};
use shared::price_estimation::PriceEstimationError;
use std::fmt::Debug;
use std::{convert::Infallible, sync::Arc};
use warp::{
    hyper::StatusCode,
    reply::{json, with_status, Json, WithStatus},
    Filter, Rejection, Reply,
};

pub fn handle_all_routes(
    database: Arc<dyn TradeRetrieving>,
    orderbook: Arc<Orderbook>,
    quoter: Arc<OrderQuoter>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    // api/v1
    let create_order = (
        "/api/v1/orders",
        create_order::create_order(orderbook.clone()),
    );
    let get_orders = ("/api/v1/orders", get_orders::get_orders(orderbook.clone()));
    let get_fee_info = (
        "/api/v1/fee",
        get_fee_info::get_fee_info(quoter.fee_calculator.clone()),
    );
    let get_order_by_uid = (
        "/api/v1/orders/*",
        get_order_by_uid::get_order_by_uid(orderbook.clone()),
    );
    let get_solvable_orders = (
        "/api/v1/solvable_orders",
        get_solvable_orders::get_solvable_orders(orderbook.clone()),
    );
    let get_trades = ("/api/v1/trades", get_trades::get_trades(database));
    let cancel_order = (
        "/api/v1/orders/*",
        cancel_order::cancel_order(orderbook.clone()),
    );
    let get_markets = (
        "/api/v1/markets/*/*/*",
        get_markets::get_amount_estimate(quoter.price_estimator.clone()),
    );
    let get_fee_and_quote_sell = (
        "/api/v1/feeAndQuote/sell",
        get_fee_and_quote::get_fee_and_quote_sell(quoter.clone()),
    );
    let get_fee_and_quote_buy = (
        "/api/v1/feeAndQuote/buy",
        get_fee_and_quote::get_fee_and_quote_buy(quoter.clone()),
    );
    let get_user_orders = (
        "/api/v1/account/*/orders",
        get_user_orders::get_user_orders(orderbook.clone()),
    );
    let get_orders_by_tx = (
        "/api/v1/transactions/*/orders",
        get_orders_by_tx::get_orders_by_tx(orderbook.clone()),
    );
    let post_quote = ("/api/v1/quote", post_quote::post_quote(quoter));

    // api/v2
    let get_solvable_orders_v2 = (
        "/api/v2/solvable_orders",
        get_solvable_orders_v2::get_solvable_orders(orderbook),
    );

    let cors = warp::cors()
        .allow_any_origin()
        .allow_methods(vec!["GET", "POST", "DELETE", "OPTIONS", "PUT", "PATCH"])
        .allow_headers(vec!["Origin", "Content-Type", "X-Auth-Token", "X-AppId"]);

    let v1 = warp::path!("api" / "v1" / ..).and(
        (create_order.1)
            .or(get_orders.1)
            .or(get_fee_info.1)
            .or(get_order_by_uid.1)
            .or(get_solvable_orders.1)
            .or(get_trades.1)
            .or(cancel_order.1)
            .or(get_markets.1)
            .or(get_fee_and_quote_sell.1)
            .or(get_fee_and_quote_buy.1)
            .or(get_user_orders.1)
            .or(get_orders_by_tx.1)
            .or(post_quote.1),
    );

    let v2 = warp::path!("api" / "v1" / ..).and(get_solvable_orders_v2.1);

    let routes = v1.or(v2);

    routes
        .recover(handle_rejection)
        .with(cors)
        .with(metrics::handle_metrics([
            create_order.0,
            get_orders.0,
            get_fee_info.0,
            get_order_by_uid.0,
            get_solvable_orders.0,
            get_trades.0,
            cancel_order.0,
            get_markets.0,
            get_fee_and_quote_sell.0,
            get_fee_and_quote_buy.0,
            get_user_orders.0,
            get_orders_by_tx.0,
            post_quote.0,
            get_solvable_orders_v2.0,
        ]))
        .with(warp::log::custom(|info| {
            tracing::info!(
                "{} \"{}\" {} {:?}",
                info.method(),
                info.path(),
                info.status().as_str(),
                info.elapsed(),
            );
        }))
}

pub type ApiReply = warp::reply::WithStatus<warp::reply::Json>;

// We turn Rejection into Reply to workaround warp not setting CORS headers on rejections.
async fn handle_rejection(err: Rejection) -> Result<impl Reply, Infallible> {
    Ok(err.default_response())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Error<'a> {
    error_type: &'a str,
    description: &'a str,
}

fn error(error_type: &str, description: impl AsRef<str>) -> Json {
    json(&Error {
        error_type,
        description: description.as_ref(),
    })
}

fn internal_error(error: anyhowError) -> Json {
    tracing::error!(?error, "internal server error");
    json(&Error {
        error_type: "InternalServerError",
        description: "",
    })
}

pub fn convert_json_response<T, E>(result: Result<T, E>) -> WithStatus<Json>
where
    T: Serialize,
    E: IntoWarpReply + Debug,
{
    match result {
        Ok(response) => with_status(warp::reply::json(&response), StatusCode::OK),
        Err(err) => err.into_warp_reply(),
    }
}

pub trait IntoWarpReply {
    fn into_warp_reply(self) -> ApiReply;
}

impl IntoWarpReply for anyhowError {
    fn into_warp_reply(self) -> ApiReply {
        with_status(internal_error(self), StatusCode::INTERNAL_SERVER_ERROR)
    }
}

impl IntoWarpReply for PriceEstimationError {
    fn into_warp_reply(self) -> WithStatus<Json> {
        match self {
            Self::UnsupportedToken(token) => with_status(
                error("UnsupportedToken", format!("Token address {:?}", token)),
                StatusCode::BAD_REQUEST,
            ),
            Self::NoLiquidity => with_status(
                error("NoLiquidity", "not enough liquidity"),
                StatusCode::NOT_FOUND,
            ),
            Self::ZeroAmount => with_status(
                error("ZeroAmount", "Please use non-zero amount field"),
                StatusCode::BAD_REQUEST,
            ),
            Self::Other(err) => with_status(
                internal_error(err.context("price_estimation")),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        }
    }
}

#[cfg(test)]
async fn response_body(response: warp::hyper::Response<warp::hyper::Body>) -> Vec<u8> {
    let mut body = response.into_body();
    let mut result = Vec::new();
    while let Some(bytes) = futures::StreamExt::next(&mut body).await {
        result.extend_from_slice(bytes.unwrap().as_ref());
    }
    result
}

const MAX_JSON_BODY_PAYLOAD: u64 = 1024 * 16;

fn extract_payload<T: DeserializeOwned + Send>(
) -> impl Filter<Extract = (T,), Error = Rejection> + Clone {
    // (rejecting huge payloads)...
    warp::body::content_length_limit(MAX_JSON_BODY_PAYLOAD).and(warp::body::json())
}
