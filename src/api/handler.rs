use super::api::SignedOrder;
use crate::models::{Order, OrderBook, Serializable_OrderBook};
use anyhow::Result;
use warp::http;

pub async fn add_order(
    order: SignedOrder,
    orderbook: OrderBook,
) -> Result<impl warp::Reply, warp::Rejection> {
    let order: Order = Order::from(order);
    if !order.validate_order().unwrap_or(false) {
        Ok(warp::reply::with_status(
            "Order does not have a valid signature",
            http::StatusCode::BAD_REQUEST,
        ))
    } else {
        let add_order_success = orderbook.add_order(order.clone()).await;
        if add_order_success {
            Ok(warp::reply::with_status(
                "Added order to the orderbook",
                http::StatusCode::CREATED,
            ))
        } else {
            Ok(warp::reply::with_status(
                "Did not add order to the orderbook, as it was already in the orderbook",
                http::StatusCode::BAD_REQUEST,
            ))
        }
    }
}

pub async fn get_orders(orderbook: OrderBook) -> Result<impl warp::Reply, warp::Rejection> {
    let orderbook_struct = Serializable_OrderBook::new(orderbook.orders.read().await.clone());
    Ok(warp::reply::json(&orderbook_struct))
}
