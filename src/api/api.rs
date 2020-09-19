use crate::models::order::Order;
use crate::models::orderbook::OrderBook;
use crate::models::tokenlist::TokenList;
use std::collections::HashMap;
use warp::{http, Filter};

async fn add_order(
    order: Order,
    state: (OrderBook, TokenList),
) -> Result<impl warp::Reply, warp::Rejection> {
    state.1.add_token(order.sell_token).await;
    state.1.add_token(order.buy_token).await;
    let current_orderbook = state.0.orderbook.read();
    let empty_hash_map = HashMap::new();
    let mut new_mash_map = current_orderbook
        .get(&order.sell_token)
        .unwrap_or(&empty_hash_map)
        .clone();
    new_mash_map.insert(order.buy_token, order.clone());
    state
        .0
        .orderbook
        .write()
        .insert(order.sell_token, new_mash_map.clone());

    Ok(warp::reply::with_status(
        "Added order to the orderbook",
        http::StatusCode::CREATED,
    ))
}
async fn get_orders(state: (OrderBook, TokenList)) -> Result<impl warp::Reply, warp::Rejection> {
    let mut result = HashMap::new();
    let r = state.0.orderbook.read();

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
pub async fn api_start(orderbook: OrderBook, token_list: TokenList) {
    let orderbook_filter = warp::any().map(move || (orderbook.clone(), token_list.clone()));
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
