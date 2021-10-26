use crate::{api::WarpReplyConverting, fee::MinFeeCalculating};
use anyhow::Result;
use chrono::{DateTime, Utc};
use model::{order::OrderKind, u256_decimal};
use primitive_types::{H160, U256};
use serde::{Deserialize, Serialize};
use shared::price_estimation::PriceEstimationError;
use std::convert::Infallible;
use std::sync::Arc;
use warp::{hyper::StatusCode, reply, Filter, Rejection, Reply};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FeeInfo {
    pub expiration_date: DateTime<Utc>,
    #[serde(with = "u256_decimal")]
    pub amount: U256,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Query {
    sell_token: H160,
    buy_token: H160,
    #[serde(with = "u256_decimal")]
    amount: U256,
    kind: OrderKind,
}

fn get_fee_info_request() -> impl Filter<Extract = (Query,), Error = Rejection> + Clone {
    warp::path!("fee")
        .and(warp::get())
        .and(warp::query::<Query>())
}

pub fn get_fee_info_response(
    result: Result<(U256, DateTime<Utc>), PriceEstimationError>,
) -> impl Reply {
    match result {
        Ok((amount, expiration_date)) => {
            let fee_info = FeeInfo {
                expiration_date,
                amount,
            };
            Ok(reply::with_status(reply::json(&fee_info), StatusCode::OK))
        }
        Err(err) => Ok(err.into_warp_reply()),
    }
}

pub fn get_fee_info(
    fee_calculator: Arc<dyn MinFeeCalculating>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    get_fee_info_request().and_then(move |query: Query| {
        let fee_calculator = fee_calculator.clone();
        async move {
            Result::<_, Infallible>::Ok(get_fee_info_response(
                fee_calculator
                    .compute_subsidized_min_fee(
                        query.sell_token,
                        Some(query.buy_token),
                        Some(query.amount),
                        Some(query.kind),
                        None,
                    )
                    .await,
            ))
        }
    })
}

// TODO remove legacy fee endpoint once frontend is updated

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyFeeInfo {
    pub expiration_date: DateTime<Utc>,
    #[serde(with = "u256_decimal")]
    pub minimal_fee: U256,
    pub fee_ratio: u32,
}

pub fn legacy_get_fee_info_request() -> impl Filter<Extract = (H160,), Error = Rejection> + Clone {
    warp::path!("tokens" / H160 / "fee").and(warp::get())
}

pub fn legacy_get_fee_info_response(
    result: Result<(U256, DateTime<Utc>), PriceEstimationError>,
) -> impl Reply {
    match result {
        Ok((minimal_fee, expiration_date)) => {
            let fee_info = LegacyFeeInfo {
                expiration_date,
                minimal_fee,
                fee_ratio: 0u32,
            };
            Ok(reply::with_status(reply::json(&fee_info), StatusCode::OK))
        }
        Err(err) => Ok(err.into_warp_reply()),
    }
}

pub fn legacy_get_fee_info(
    fee_calculator: Arc<dyn MinFeeCalculating>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    legacy_get_fee_info_request().and_then(move |token| {
        let fee_calculator = fee_calculator.clone();
        async move {
            Result::<_, Infallible>::Ok(legacy_get_fee_info_response(
                fee_calculator
                    .compute_subsidized_min_fee(token, None, None, None, None)
                    .await,
            ))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::response_body;
    use chrono::FixedOffset;
    use warp::test::request;

    #[tokio::test]
    async fn get_fee_info_request_ok() {
        let filter = get_fee_info_request();
        let sell_token = String::from("0x0000000000000000000000000000000000000001");
        let buy_token = String::from("0x0000000000000000000000000000000000000002");
        let path_string = format!(
            "/fee?sellToken={}&buyToken={}&amount={}&kind=buy",
            sell_token,
            buy_token,
            U256::exp10(18)
        );
        let request = request().path(&path_string).method("GET");
        let result = request.filter(&filter).await.unwrap();
        assert_eq!(result.sell_token, H160::from_low_u64_be(1));
        assert_eq!(result.buy_token, H160::from_low_u64_be(2));
        assert_eq!(result.amount, U256::exp10(18));
        assert_eq!(result.kind, OrderKind::Buy);
    }

    #[tokio::test]
    async fn get_fee_info_response_() {
        let response =
            get_fee_info_response(Ok((U256::zero(), Utc::now() + FixedOffset::east(10))))
                .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_body(response).await;
        let body: FeeInfo = serde_json::from_slice(body.as_slice()).unwrap();
        assert_eq!(body.amount, U256::zero());
        assert!(body.expiration_date.gt(&chrono::offset::Utc::now()))
    }

    #[tokio::test]
    async fn legacy_get_fee_info_request_ok() {
        let filter = legacy_get_fee_info_request();
        let token = String::from("0x0000000000000000000000000000000000000001");
        let path_string = format!("/tokens/{}/fee", token);
        let request = request().path(&path_string).method("GET");
        let result = request.filter(&filter).await.unwrap();
        assert_eq!(result, H160::from_low_u64_be(1));
    }

    #[tokio::test]
    async fn legacy_get_fee_info_response_() {
        let response =
            legacy_get_fee_info_response(Ok((U256::zero(), Utc::now() + FixedOffset::east(10))))
                .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_body(response).await;
        let body: LegacyFeeInfo = serde_json::from_slice(body.as_slice()).unwrap();
        assert_eq!(body.minimal_fee, U256::zero());
        assert_eq!(body.fee_ratio, 0);
        assert!(body.expiration_date.gt(&chrono::offset::Utc::now()))
    }
}
