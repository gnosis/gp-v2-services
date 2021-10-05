use crate::orderbook::Orderbook;
use anyhow::Result;
use model::order::Order;
use shared::H256Wrapper;
use std::{convert::Infallible, sync::Arc};
use warp::{hyper::StatusCode, reply, Filter, Rejection, Reply};

pub fn get_orders_by_tx_request() -> impl Filter<Extract = (H256Wrapper,), Error = Rejection> + Clone
{
    warp::path!("orders" / H256Wrapper).and(warp::get())
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
    get_orders_by_tx_request().and_then(move |hash: H256Wrapper| {
        let orderbook = orderbook.clone();
        async move {
            let result = orderbook.get_orders_for_tx(&hash.0).await;
            Result::<_, Infallible>::Ok(get_orders_by_tx_response(result))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::response_body;
    use std::str::FromStr;
    use warp::test::request;

    #[tokio::test]
    async fn request_ok() {
        let hash = H256Wrapper::from_str("0x0191dbb560e936bd3320d5a505c9c05580a0ebb7e12fe117551ac26e484f295e").unwrap();
        let request = request()
            .path(&format!("/orders/{:?}", hash))
            .method("GET");
        let filter = get_orders_by_tx_request();
        let result = request.filter(&filter).await.unwrap();
        assert_eq!(result.0, hash.0);
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
}
