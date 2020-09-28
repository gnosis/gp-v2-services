use super::filter::get;
use super::filter::post_order;
use crate::models::OrderBook;
use core::future::Future;
use warp::Filter;

pub fn run_api(orderbook: OrderBook) -> impl Future<Output = ()> + 'static {
    let routes = post_order(orderbook.clone()).or(get(orderbook.clone()));
    warp::serve(routes).bind(([127, 0, 0, 1], 3030))
}
