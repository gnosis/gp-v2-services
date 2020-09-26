use super::api::SignedOrder;
use super::handler::{add_order, get_orders};
use crate::models::OrderBook;
use warp::Filter;

fn json_body() -> impl Filter<Extract = (SignedOrder,), Error = warp::Rejection> + Clone {
    // When accepting a body, we want a JSON body
    // (and to reject huge payloads)...
    warp::body::content_length_limit(1024 * 16).and(warp::body::json())
}

fn with_orderbook(
    orderbook: OrderBook,
) -> impl Filter<Extract = (OrderBook,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || orderbook.clone())
}

pub fn post_order(
    orderbook: OrderBook,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::post()
        .and(warp::path("v1"))
        .and(warp::path("orders"))
        .and(warp::path::end())
        .and(json_body())
        .and(with_orderbook(orderbook))
        .and_then(add_order)
}

pub fn get(
    orderbook: OrderBook,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::get()
        .and(warp::path("v1"))
        .and(warp::path("orders"))
        .and(warp::path::end())
        .and(warp::path::end())
        .and(with_orderbook(orderbook))
        .and_then(get_orders)
}
