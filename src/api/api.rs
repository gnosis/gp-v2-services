use crate::models::order::Order;
use crate::models::orderbook::OrderBook;
use crate::models::token_list::TokenList;
use anyhow::Result;
use core::future::Future;
use std::collections::HashMap;
use warp::{http, Filter};
pub struct State {
    orderbook: OrderBook,
    token_list: TokenList,
}

async fn add_order(order: Order, state: State) -> Result<impl warp::Reply, warp::Rejection> {
    state.token_list.add_token(order.sell_token).await;
    state.token_list.add_token(order.buy_token).await;
    let empty_hash_map = HashMap::new();
    // if we are not cloning here, the write operation is blocked
    // todo: find better solution
    let current_orderbook = state.orderbook.orders.read().clone();
    let empty_hash_vec: Vec<Order> = Vec::new();
    let new_hash_map = current_orderbook
        .get(&order.sell_token.clone())
        .unwrap_or(&empty_hash_map);
    let mut new_vec = new_hash_map
        .get(&order.buy_token)
        .unwrap_or(&empty_hash_vec)
        .clone();
    new_vec.push(order.clone());
    let mut hash_map = new_hash_map.clone();
    let pos = new_vec.binary_search(&order.clone()).unwrap_or_else(|e| e);
    new_vec.insert(pos, order.clone());
    hash_map.insert(order.buy_token, new_vec);
    state
        .orderbook
        .orders
        .write()
        .insert(order.sell_token, hash_map.clone());

    Ok(warp::reply::with_status(
        "Added order to the orderbook",
        http::StatusCode::CREATED,
    ))
}

async fn get_orders(state: State) -> Result<impl warp::Reply, warp::Rejection> {
    let mut result = HashMap::new();
    let r = state.orderbook.orders.read();

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

pub fn run_api(orderbook: OrderBook, token_list: TokenList) -> impl Future<Output = ()> + 'static {
    let orderbook_filter = warp::any().map(move || State {
        orderbook: orderbook.clone(),
        token_list: token_list.clone(),
    });
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
    warp::serve(routes).bind(([127, 0, 0, 1], 3030))
}
