use crate::api::price_estimation_error_to_warp_reply;
use crate::{
    api::post_quote::{OrderQuoteRequest, OrderQuoteSide, SellAmount},
    fee::MinFeeCalculating,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use ethcontract::{H160, U256};
use model::order::OrderKind;
use model::{h160_hexadecimal, u256_decimal};
use serde::{Deserialize, Serialize};
use shared::price_estimation::{PriceEstimating, PriceEstimationError};
use std::{convert::Infallible, sync::Arc};
use warp::{
    hyper::StatusCode,
    reply::{self, Json},
    Filter, Rejection, Reply,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fee {
    #[serde(with = "u256_decimal")]
    pub amount: U256,
    pub expiration_date: DateTime<Utc>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FeeParameters {
    pub buy_amount: U256,
    pub sell_amount: U256,
    pub fee_amount: U256,
    pub expiration: DateTime<Utc>,
    pub kind: OrderKind,
}

#[derive(Debug)]
pub enum FeeError {
    SellAmountDoesNotCoverFee,
    PriceEstimate(PriceEstimationError),
}

impl FeeError {
    pub fn to_warp_reply(&self) -> (Json, StatusCode) {
        match self {
            FeeError::PriceEstimate(err) => price_estimation_error_to_warp_reply(err.clone()),
            FeeError::SellAmountDoesNotCoverFee => (
                super::error(
                    "SellAmountDoesNotCoverFee",
                    "The sell amount for the sell order is lower than the fee.".to_string(),
                ),
                StatusCode::BAD_REQUEST,
            ),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SellQuery {
    #[serde(with = "h160_hexadecimal")]
    sell_token: H160,
    #[serde(with = "h160_hexadecimal")]
    buy_token: H160,
    // The total amount to be sold from which the fee will be deducted.
    #[serde(with = "u256_decimal")]
    sell_amount_before_fee: U256,
}

impl From<SellQuery> for OrderQuoteRequest {
    fn from(query: SellQuery) -> Self {
        let side = OrderQuoteSide::Sell {
            sell_amount: SellAmount::BeforeFee {
                value: query.sell_amount_before_fee,
            },
        };
        OrderQuoteRequest::new(query.sell_token, query.buy_token, side)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SellResponse {
    // The fee that is deducted from sell_amount_before_fee. The sell amount that is traded is
    // sell_amount_before_fee - fee_in_sell_token.
    fee: Fee,
    // The expected buy amount for the traded sell amount.
    #[serde(with = "u256_decimal")]
    buy_amount_after_fee: U256,
}

impl From<FeeParameters> for SellResponse {
    fn from(fee_parameters: FeeParameters) -> Self {
        Self {
            fee: Fee {
                amount: fee_parameters.fee_amount,
                expiration_date: fee_parameters.expiration,
            },
            buy_amount_after_fee: fee_parameters.buy_amount,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuyQuery {
    #[serde(with = "h160_hexadecimal")]
    sell_token: H160,
    #[serde(with = "h160_hexadecimal")]
    buy_token: H160,
    // The total amount to be bought.
    #[serde(with = "u256_decimal")]
    buy_amount_after_fee: U256,
}

impl From<BuyQuery> for OrderQuoteRequest {
    fn from(query: BuyQuery) -> Self {
        let side = OrderQuoteSide::Buy {
            buy_amount_after_fee: query.buy_amount_after_fee,
        };
        OrderQuoteRequest::new(query.sell_token, query.buy_token, side)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BuyResponse {
    // The fee that is deducted from sell_amount_before_fee. The sell amount that is traded is
    // sell_amount_before_fee - fee_in_sell_token.
    fee: Fee,
    #[serde(with = "u256_decimal")]
    sell_amount_before_fee: U256,
}

impl From<FeeParameters> for BuyResponse {
    fn from(fee_parameters: FeeParameters) -> Self {
        Self {
            fee: Fee {
                amount: fee_parameters.fee_amount,
                expiration_date: fee_parameters.expiration,
            },
            sell_amount_before_fee: fee_parameters.sell_amount,
        }
    }
}

fn sell_request() -> impl Filter<Extract = (SellQuery,), Error = Rejection> + Clone {
    warp::path!("feeAndQuote" / "sell")
        .and(warp::get())
        .and(warp::query::<SellQuery>())
}

fn buy_request() -> impl Filter<Extract = (BuyQuery,), Error = Rejection> + Clone {
    warp::path!("feeAndQuote" / "buy")
        .and(warp::get())
        .and(warp::query::<BuyQuery>())
}

fn response<T: Serialize>(result: Result<T, FeeError>) -> impl Reply {
    match result {
        Ok(response) => reply::with_status(reply::json(&response), StatusCode::OK),
        Err(FeeError::SellAmountDoesNotCoverFee) => reply::with_status(
            super::error(
                "SellAmountDoesNotCoverFee",
                "The sell amount for the sell order is lower than the fee.".to_string(),
            ),
            StatusCode::BAD_REQUEST,
        ),
        Err(FeeError::PriceEstimate(err)) => {
            let (json, status_code) = price_estimation_error_to_warp_reply(err);
            reply::with_status(json, status_code)
        }
    }
}

pub fn get_fee_and_quote_sell(
    fee_calculator: Arc<dyn MinFeeCalculating>,
    price_estimator: Arc<dyn PriceEstimating>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    sell_request().and_then(move |query| {
        let fee_calculator = fee_calculator.clone();
        let price_estimator = price_estimator.clone();
        async move {
            Result::<_, Infallible>::Ok(response(
                OrderQuoteRequest::from(query)
                    .calculate_fee_parameters(fee_calculator, price_estimator)
                    .await
                    .map(SellResponse::from),
            ))
        }
    })
}

pub fn get_fee_and_quote_buy(
    fee_calculator: Arc<dyn MinFeeCalculating>,
    price_estimator: Arc<dyn PriceEstimating>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    buy_request().and_then(move |query| {
        let fee_calculator = fee_calculator.clone();
        let price_estimator = price_estimator.clone();
        async move {
            Result::<_, Infallible>::Ok(response(
                OrderQuoteRequest::from(query)
                    .calculate_fee_parameters(fee_calculator, price_estimator)
                    .await
                    .map(BuyResponse::from),
            ))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;
    use hex_literal::hex;
    use warp::test::request;

    #[test]
    fn sell_query() {
        let path= "/feeAndQuote/sell?sellToken=0xdac17f958d2ee523a2206206994597c13d831ec7&buyToken=0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48&sellAmountBeforeFee=1000000";
        let request = request().path(path).method("GET");
        let result = request
            .filter(&sell_request())
            .now_or_never()
            .unwrap()
            .unwrap();
        assert_eq!(
            result.sell_token,
            H160(hex!("dac17f958d2ee523a2206206994597c13d831ec7"))
        );
        assert_eq!(
            result.buy_token,
            H160(hex!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"))
        );
        assert_eq!(result.sell_amount_before_fee, 1000000.into());
    }

    #[test]
    fn buy_query() {
        let path= "/feeAndQuote/buy?sellToken=0xdac17f958d2ee523a2206206994597c13d831ec7&buyToken=0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48&buyAmountAfterFee=1000000";
        let request = request().path(path).method("GET");
        let result = request
            .filter(&buy_request())
            .now_or_never()
            .unwrap()
            .unwrap();
        assert_eq!(
            result.sell_token,
            H160(hex!("dac17f958d2ee523a2206206994597c13d831ec7"))
        );
        assert_eq!(
            result.buy_token,
            H160(hex!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"))
        );
        assert_eq!(result.buy_amount_after_fee, 1000000.into());
    }
}
