use crate::api;
use anyhow::{anyhow, Result};
use ethcontract::{H160, U256};
use model::{
    appdata_hexadecimal,
    order::{BuyTokenDestination, OrderKind, SellTokenSource},
    u256_decimal,
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use warp::{hyper::StatusCode, reply, Filter, Rejection, Reply};

/// The order parameters to quote a price and fee for.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrderQuoteRequest {
    from: H160,
    sell_token: H160,
    buy_token: H160,
    receiver: Option<H160>,
    #[serde(flatten)]
    side: OrderQuoteSide,
    valid_to: u32,
    #[serde(with = "appdata_hexadecimal")]
    app_data: [u8; 32],
    partially_fillable: bool,
    #[serde(default)]
    sell_token_balance: SellTokenSource,
    #[serde(default)]
    buy_token_balance: BuyTokenDestination,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
enum OrderQuoteSide {
    #[serde(rename_all = "camelCase")]
    Sell {
        #[serde(with = "u256_decimal")]
        total_sell_amount: U256,
    },
    #[serde(rename_all = "camelCase")]
    Buy {
        #[serde(with = "u256_decimal")]
        buy_amount: U256,
    },
}

/// The quoted order by the service.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OrderQuote {
    from: H160,
    sell_token: H160,
    buy_token: H160,
    receiver: Option<H160>,
    #[serde(with = "u256_decimal")]
    sell_amount: U256,
    #[serde(with = "u256_decimal")]
    buy_amount: U256,
    valid_to: u32,
    #[serde(with = "appdata_hexadecimal")]
    app_data: [u8; 32],
    #[serde(with = "u256_decimal")]
    fee_amount: U256,
    kind: OrderKind,
    partially_fillable: bool,
    sell_token_balance: SellTokenSource,
    buy_token_balance: BuyTokenDestination,
}

fn post_quote_request() -> impl Filter<Extract = (OrderQuoteRequest,), Error = Rejection> + Clone {
    warp::path!("feeAndQuote" / "sell")
        .and(warp::post())
        .and(api::extract_payload())
}

fn post_order_response(result: Result<OrderQuote>) -> impl Reply {
    match result {
        Ok(response) => reply::with_status(reply::json(&response), StatusCode::OK),
        Err(err) => reply::with_status(
            super::error("NotYetImplemented", err.to_string()),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub fn post_quote() -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    post_quote_request().and_then(move |request| async move {
        tracing::warn!("unimplemented request {:#?}", request);
        Result::<_, Infallible>::Ok(post_order_response(Err(anyhow!("not yet implemented"))))
    })
}
