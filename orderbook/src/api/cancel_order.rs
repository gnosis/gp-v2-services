use crate::orderbook::{OrderCancellationResult, Orderbook};
use anyhow::Result;
use model::order::OrderCancellation;
use std::{convert::Infallible, sync::Arc};
use warp::{hyper::StatusCode, Filter, Rejection, Reply};

const MAX_JSON_BODY_PAYLOAD: u64 = 1024 * 16;

fn extract_cancellation() -> impl Filter<Extract = (OrderCancellation,), Error = Rejection> + Clone
{
    // (rejecting huge payloads)...
    warp::body::content_length_limit(MAX_JSON_BODY_PAYLOAD).and(warp::body::json())
}

pub fn cancel_order_request(
) -> impl Filter<Extract = (OrderCancellation,), Error = Rejection> + Clone {
    warp::path!("orders")
        .and(warp::delete())
        .and(extract_cancellation())
}

pub fn cancel_order_response(result: Result<OrderCancellationResult>) -> impl Reply {
    let (body, status_code) = match result {
        Ok(OrderCancellationResult::Cancelled) => {
            (warp::reply::json(&"Cancelled"), StatusCode::ACCEPTED)
        }
        Ok(OrderCancellationResult::InvalidSignature) => (
            super::error("InvalidSignature", "Likely malformed signature"),
            StatusCode::BAD_REQUEST,
        ),
        Ok(OrderCancellationResult::OrderNotFound) => (
            super::error("OrderNotFound", "order not located in database"),
            StatusCode::BAD_REQUEST,
        ),
        Ok(OrderCancellationResult::WrongOwner) => (
            super::error(
                "WrongOwner",
                "Signature recovery's owner doesn't match order's",
            ),
            StatusCode::BAD_REQUEST,
        ),
        Err(_) => (super::internal_error(), StatusCode::INTERNAL_SERVER_ERROR),
    };
    warp::reply::with_status(body, status_code)
}

pub fn cancel_order(
    orderbook: Arc<Orderbook>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    cancel_order_request().and_then(move |order| {
        let orderbook = orderbook.clone();
        async move {
            let result = orderbook.cancel_order(order).await;
            if let Err(err) = &result {
                tracing::error!(?err, ?order, "cancel_order error");
            }
            Result::<_, Infallible>::Ok(cancel_order_response(result))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use warp::test::request;

    #[tokio::test]
    async fn cancel_order_request_ok() {
        let filter = cancel_order_request();
        let cancellation = OrderCancellation::default();
        let request = request()
            .path("/orders")
            .method("DELETE")
            .header("content-type", "application/json")
            .json(&cancellation);
        let result = request.filter(&filter).await.unwrap();
        assert_eq!(result, cancellation);
    }
}
