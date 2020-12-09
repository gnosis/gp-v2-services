mod filter;
mod handler;

use crate::orderbook::OrderBook;
use std::sync::Arc;
use warp::Filter;

pub fn handle_all_routes(
    orderbook: Arc<OrderBook>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    let order_creation = filter::create_order(orderbook.clone());
    let order_getter = filter::get_orders(orderbook);
    let fee_info = filter::get_fee_info();

    let label = |label: &'static str| warp::any().map(move || label);
    let routes_with_labels = warp::path!("api" / "v1" / ..).and(
        (label("order_creation").and(order_creation))
            .or(label("order_getter").and(order_getter))
            .unify()
            .or(label("fee_info").and(fee_info))
            .unify(),
    );
    warp::any().and(routes_with_labels.map(|_, result| result))
}
