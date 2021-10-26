use crate::{
    api::{extract_payload, IntoWarpReply},
    orderbook::{OrderCancellationResult, Orderbook},
};
use anyhow::Result;
use model::{
    order::{OrderCancellation, OrderUid},
    signature::{EcdsaSignature, EcdsaSigningScheme},
};
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, sync::Arc};
use warp::{
    hyper::StatusCode,
    reply::{with_status, Json, WithStatus},
    Filter, Rejection, Reply,
};

#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CancellationPayload {
    signature: EcdsaSignature,
    signing_scheme: EcdsaSigningScheme,
}

pub fn cancel_order_request(
) -> impl Filter<Extract = (OrderCancellation,), Error = Rejection> + Clone {
    warp::path!("orders" / OrderUid)
        .and(warp::delete())
        .and(extract_payload())
        .map(|uid, payload: CancellationPayload| OrderCancellation {
            order_uid: uid,
            signature: payload.signature,
            signing_scheme: payload.signing_scheme,
        })
}

impl IntoWarpReply for OrderCancellationResult {
    fn into_warp_reply(self) -> WithStatus<Json> {
        match self {
            Self::Cancelled => with_status(warp::reply::json(&"Cancelled"), StatusCode::OK),
            Self::InvalidSignature => with_status(
                super::error("InvalidSignature", "Likely malformed signature"),
                StatusCode::BAD_REQUEST,
            ),
            Self::AlreadyCancelled => with_status(
                super::error("AlreadyCancelled", "Order is already cancelled"),
                StatusCode::BAD_REQUEST,
            ),
            Self::OrderFullyExecuted => with_status(
                super::error("OrderFullyExecuted", "Order is fully executed"),
                StatusCode::BAD_REQUEST,
            ),
            Self::OrderExpired => with_status(
                super::error("OrderExpired", "Order is expired"),
                StatusCode::BAD_REQUEST,
            ),
            Self::OrderNotFound => with_status(
                super::error("OrderNotFound", "Order not located in database"),
                StatusCode::NOT_FOUND,
            ),
            Self::WrongOwner => with_status(
                super::error(
                    "WrongOwner",
                    "Signature recovery's owner doesn't match order's",
                ),
                StatusCode::UNAUTHORIZED,
            ),
            Self::OnChainOrder => with_status(
                super::error("OnChainOrder", "On-chain orders must be cancelled on-chain"),
                StatusCode::BAD_REQUEST,
            ),
        }
    }
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
            Result::<_, Infallible>::Ok(match result {
                Ok(result) => result.into_warp_reply(),
                Err(_) => with_status(super::internal_error(), StatusCode::INTERNAL_SERVER_ERROR),
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethcontract::H256;
    use hex_literal::hex;
    use serde_json::json;
    use warp::test::request;

    #[test]
    fn cancellation_payload_deserialization() {
        assert_eq!(
            CancellationPayload::deserialize(json!({
                "signature": "0x\
                    000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
                    202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f\
                    1b",
                "signingScheme": "eip712"
            }))
            .unwrap(),
            CancellationPayload {
                signature: EcdsaSignature {
                    r: H256(hex!(
                        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
                    )),
                    s: H256(hex!(
                        "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f"
                    )),
                    v: 27,
                },
                signing_scheme: EcdsaSigningScheme::Eip712,
            },
        );
    }

    #[tokio::test]
    async fn cancel_order_request_ok() {
        let filter = cancel_order_request();
        let cancellation = OrderCancellation::default();

        let request = request()
            .path(&format!("/orders/{:}", cancellation.order_uid))
            .method("DELETE")
            .header("content-type", "application/json")
            .json(&CancellationPayload {
                signature: cancellation.signature,
                signing_scheme: cancellation.signing_scheme,
            });
        let result = request.filter(&filter).await.unwrap();
        assert_eq!(result, cancellation);
    }
}
