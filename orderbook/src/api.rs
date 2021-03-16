mod cancel_order;
mod create_order;
mod get_fee_info;
mod get_order_by_uid;
mod get_orders;
mod get_solver_orders;
mod get_trades;

use crate::database::Database;
use crate::{fee::MinFeeCalculator, orderbook::Orderbook};
use anyhow::Error as anyhowError;
use hex::{FromHex, FromHexError};
use model::h160_hexadecimal;
use primitive_types::H160;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::{str::FromStr, sync::Arc};
use warp::{
    hyper::StatusCode,
    reply::{json, with_status, Json, WithStatus},
    Filter, Rejection, Reply,
};

pub fn handle_all_routes(
    database: Database,
    orderbook: Arc<Orderbook>,
    fee_calculator: Arc<MinFeeCalculator>,
) -> impl Filter<Extract = (impl Reply,), Error = warp::Rejection> + Clone {
    let create_order = create_order::create_order(orderbook.clone());
    let get_orders = get_orders::get_orders(orderbook.clone());
    let legacy_fee_info = get_fee_info::legacy_get_fee_info(fee_calculator.clone());
    let fee_info = get_fee_info::get_fee_info(fee_calculator);
    let get_order = get_order_by_uid::get_order_by_uid(orderbook.clone());
    let get_solvable_orders = get_solver_orders::get_solver_orders(orderbook.clone());
    let get_trades = get_trades::get_trades(database);
    let cancel_order = cancel_order::cancel_order(orderbook);
    warp::path!("api" / "v1" / ..).and(
        create_order
            .or(get_orders)
            .or(fee_info)
            .or(legacy_fee_info)
            .or(get_order)
            .or(get_solvable_orders)
            .or(get_trades)
            .or(cancel_order),
    )
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

fn internal_error() -> Json {
    json(&Error {
        error_type: "InternalServerError",
        description: "",
    })
}

pub fn convert_get_orders_error_to_reply(err: anyhowError) -> WithStatus<Json> {
    tracing::error!(?err, "get_orders error");
    with_status(internal_error(), StatusCode::INTERNAL_SERVER_ERROR)
}

pub fn convert_get_trades_error_to_reply(err: anyhowError) -> WithStatus<Json> {
    tracing::error!(?err, "get_trades error");
    with_status(internal_error(), StatusCode::INTERNAL_SERVER_ERROR)
}

/// Wraps H160 with FromStr and Deserialize that can handle a `0x` prefix.
#[derive(Deserialize)]
#[serde(transparent)]
struct H160Wrapper(#[serde(with = "h160_hexadecimal")] H160);
impl FromStr for H160Wrapper {
    type Err = FromHexError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        Ok(H160Wrapper(H160(FromHex::from_hex(s)?)))
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
