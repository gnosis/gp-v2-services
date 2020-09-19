use crate::models::order::Order;
use crate::models::orderbook::OrderBook;
use std::collections::HashMap;
use warp::{http, Filter};

async fn add_order(
    order: Order,
    orderbook: OrderBook,
) -> Result<impl warp::Reply, warp::Rejection> {
    let current_orderbook = orderbook.orderbook.read();
    let empty_hash_map = HashMap::new();
    let mut new_mash_map = current_orderbook
        .get(&order.sell_token)
        .unwrap_or(&empty_hash_map)
        .clone();
    new_mash_map.insert(order.buy_token, order.clone());
    orderbook
        .orderbook
        .write()
        .insert(order.sell_token, new_mash_map.clone());

    Ok(warp::reply::with_status(
        "Added order to the orderbook",
        http::StatusCode::CREATED,
    ))
}
async fn get_orders(orderbook: OrderBook) -> Result<impl warp::Reply, warp::Rejection> {
    let mut result = HashMap::new();
    let r = orderbook.orderbook.read();

    for (key, value) in r.iter() {
        result.insert(key, value);
    }

    Ok(warp::reply::json(&result))
}

fn json_body() -> impl Filter<Extract = (Order,), Error = warp::Rejection> + Clone {
    // When accepting a body, we want a JSON body
    // (and to reject huge payloads)...
    warp::body::content_length_limit(1024 * 16).and(warp::body::json())
}

#[tokio::main]
pub async fn api_start(orderbook: OrderBook) {
    let orderbook_filter = warp::any().map(move || orderbook.clone());
    let get_items = warp::get()
        .and(warp::path("v1"))
        .and(warp::path("orders"))
        .and(warp::path::end())
        .and(orderbook_filter.clone())
        .and_then(get_orders);
    let add_items = warp::post()
        .and(warp::path("v1"))
        .and(warp::path("orders"))
        .and(warp::path::end())
        .and(json_body())
        .and(orderbook_filter.clone())
        .and_then(add_order);

    let routes = add_items.or(get_items);

    warp::serve(routes).run(([127, 0, 0, 1], 3030)).await;
}
