use crate::orderbook::Orderbook;
use anyhow::Result;
use ethcontract::H256;
use model::order::Order;
use std::{convert::Infallible, sync::Arc};
use warp::{hyper::StatusCode, reply, Filter, Rejection, Reply};

pub fn get_orders_by_tx_request() -> impl Filter<Extract = (H256,), Error = Rejection> + Clone {
    warp::path!("transactions" / H256 / "orders").and(warp::get())
}

pub fn get_orders_by_tx_response(result: Result<Vec<Order>>) -> impl Reply {
    match result {
        Ok(orders) => reply::with_status(reply::json(&orders), StatusCode::OK),
        Err(err) => {
            tracing::error!(?err, "get_orders_by_tx error");
            reply::with_status(super::internal_error(), StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub fn get_orders_by_tx(
    orderbook: Arc<Orderbook>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    get_orders_by_tx_request().and_then(move |hash: H256| {
        let orderbook = orderbook.clone();
        async move {
            let result = orderbook.get_orders_for_tx(&hash).await;
            Result::<_, Infallible>::Ok(get_orders_by_tx_response(result))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::response_body;
    use crate::database::{
        events::{Event, Settlement, Trade},
        orders::OrderStoring,
        Postgres,
    };
    use model::order::{OrderMetaData, OrderUid};
    use shared::event_handling::EventIndex;
    use std::str::FromStr;

    #[tokio::test]
    async fn request_ok() {
        let hash_str = "0x0191dbb560e936bd3320d5a505c9c05580a0ebb7e12fe117551ac26e484f295e";
        let result = warp::test::request()
            .path(&format!("/transactions/{:}/orders", hash_str))
            .method("GET")
            .filter(&get_orders_by_tx_request())
            .await
            .unwrap();
        assert_eq!(result.0, H256::from_str(hash_str).unwrap().0);
    }

    #[tokio::test]
    async fn response_ok() {
        let orders = vec![Order::default()];
        let response = get_orders_by_tx_response(Ok(orders.clone())).into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_body(response).await;
        let response_orders: Vec<Order> = serde_json::from_slice(body.as_slice()).unwrap();
        assert_eq!(response_orders, orders);
    }

    #[tokio::test]
    #[ignore]
    async fn postgres_returns_expected_orders_for_tx_hash_request() {
        let db = Postgres::new("postgresql://").unwrap();
        db.clear().await.unwrap();

        let orders: Vec<Order> = (0..=3)
            .map(|i| Order {
                order_meta_data: OrderMetaData {
                    uid: OrderUid::from_integer(i),
                    ..Default::default()
                },
                order_creation: Default::default(),
            })
            .collect();
        // Add settlement
        db.append_events_(vec![(
            EventIndex {
                block_number: 0,
                log_index: 0,
            },
            Event::Settlement(Settlement {
                solver: Default::default(),
                transaction_hash: H256::from_low_u64_be(1),
            }),
        )])
        .await
        .unwrap();
        // Each order was traded in the same block.
        for (i, order) in orders.clone().iter().enumerate() {
            db.insert_order(&order).await.unwrap();
            db.append_events_(vec![(
                EventIndex {
                    block_number: 0,
                    log_index: i as u64 + 1,
                },
                Event::Trade(Trade {
                    order_uid: order.order_meta_data.uid,
                    ..Default::default()
                }),
            )])
            .await
            .unwrap();
        }

        let res = db.orders_for_tx(&H256::from_low_u64_be(1)).await.unwrap();
        assert_eq!(res, orders);
    }
}
