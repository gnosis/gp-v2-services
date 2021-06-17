use crate::fee::{MinFeeCalculating, MinFeeCalculationError};
use anyhow::Result;
use chrono::{DateTime, Utc};
use ethcontract::{H160, U256};
use model::h160_hexadecimal;
use model::{order::OrderKind, u256_decimal};
use serde::{Deserialize, Serialize};
use shared::{
    conversions::{big_int_to_u256, U256Ext},
    price_estimate::{PriceEstimating, PriceEstimationError},
};
use std::convert::Infallible;
use std::sync::Arc;
use warp::{hyper::StatusCode, reply, Filter, Rejection, Reply};

#[derive(Deserialize)]
struct Query {
    #[serde(with = "h160_hexadecimal")]
    sell_token: H160,
    #[serde(with = "h160_hexadecimal")]
    buy_token: H160,
    // For sell orders in sell token, for buy orders in the buy token.
    #[serde(with = "u256_decimal")]
    in_amount_before_fees: U256,
    kind: OrderKind,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Response {
    expiration_date: DateTime<Utc>,
    #[serde(with = "u256_decimal")]
    min_fee: U256,
    // For sell orders in buy token, for buy orders in the sell token.
    #[serde(with = "u256_decimal")]
    out_amount_before_fees: U256,
    // For buy orders this is the same as out_amount_before_fees. For sell orders this is less
    // than out_amount_before_fees because the fee is first deducted from the sell amount before
    // applying the price.
    #[serde(with = "u256_decimal")]
    out_amount_after_fees: U256,
}

#[derive(Debug)]
enum Error {
    NotFound,
    UnsupportedToken(H160),
    AmountIsZero,
    SellAmountDoesNotCoverFee,
    Other(anyhow::Error),
}

impl From<MinFeeCalculationError> for Error {
    fn from(other: MinFeeCalculationError) -> Self {
        match other {
            MinFeeCalculationError::NotFound => Error::NotFound,
            MinFeeCalculationError::UnsupportedToken(token) => Error::UnsupportedToken(token),
            MinFeeCalculationError::Other(error) => Error::Other(error),
        }
    }
}

impl From<PriceEstimationError> for Error {
    fn from(other: PriceEstimationError) -> Self {
        match other {
            PriceEstimationError::UnsupportedToken(token) => Error::UnsupportedToken(token),
            PriceEstimationError::Other(error) => Error::Other(error),
        }
    }
}

fn request() -> impl Filter<Extract = (Query,), Error = Rejection> + Clone {
    warp::path!("fee_and_quote")
        .and(warp::get())
        .and(warp::query::<Query>())
}

fn response(result: Result<Response, Error>) -> impl Reply {
    match result {
        Ok(response) => reply::with_status(reply::json(&response), StatusCode::OK),
        Err(Error::NotFound) => reply::with_status(
            super::error("NotFound", "Token was not found"),
            StatusCode::NOT_FOUND,
        ),
        Err(Error::UnsupportedToken(token)) => reply::with_status(
            super::error("UnsupportedToken", format!("Token address {:?}", token)),
            StatusCode::BAD_REQUEST,
        ),
        Err(Error::AmountIsZero) => reply::with_status(
            super::error(
                "AmountIsZero",
                "The input amount must be greater than zero.".to_string(),
            ),
            StatusCode::BAD_REQUEST,
        ),
        Err(Error::SellAmountDoesNotCoverFee) => reply::with_status(
            super::error(
                "SellAmountDoesNotCoverFee",
                "The sell amount for the sell order is lower than the fee.".to_string(),
            ),
            StatusCode::BAD_REQUEST,
        ),
        Err(Error::Other(err)) => {
            tracing::error!(?err, "get_fee_and_price error");
            reply::with_status(super::internal_error(), StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn calculate(
    fee_calculator: Arc<dyn MinFeeCalculating>,
    price_estimator: Arc<dyn PriceEstimating>,
    query: Query,
) -> Result<Response, Error> {
    if query.in_amount_before_fees.is_zero() {
        return Err(Error::AmountIsZero);
    }

    let (min_fee, expiration_date) = fee_calculator
        .min_fee(query.sell_token, None, None, None)
        .await?;
    let in_amount_after_fee = match query.kind {
        OrderKind::Buy => query.in_amount_before_fees,
        OrderKind::Sell => query
            .in_amount_before_fees
            .checked_sub(min_fee)
            .ok_or(Error::SellAmountDoesNotCoverFee)?,
    };

    let price = price_estimator
        .estimate_price(
            query.sell_token,
            query.buy_token,
            in_amount_after_fee,
            query.kind,
        )
        .await?;
    let out_amount_before_fees = big_int_to_u256(
        &match query.kind {
            OrderKind::Buy => query.in_amount_before_fees.to_big_rational() * &price,
            OrderKind::Sell => query.in_amount_before_fees.to_big_rational() / &price,
        }
        .to_integer(),
    )
    .map_err(Error::Other)?;
    let out_amount_after_fees = big_int_to_u256(
        &match query.kind {
            OrderKind::Buy => in_amount_after_fee.to_big_rational() * &price,
            OrderKind::Sell => in_amount_after_fee.to_big_rational() / &price,
        }
        .to_integer(),
    )
    .map_err(Error::Other)?;

    Ok(Response {
        expiration_date,
        min_fee,
        out_amount_before_fees,
        out_amount_after_fees,
    })
}

pub fn get_fee_and_quote(
    fee_calculator: Arc<dyn MinFeeCalculating>,
    price_estimator: Arc<dyn PriceEstimating>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    request().and_then(move |query| {
        let fee_calculator = fee_calculator.clone();
        let price_estimator = price_estimator.clone();
        async move {
            Result::<_, Infallible>::Ok(response(
                calculate(fee_calculator, price_estimator, query).await,
            ))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fee::MockMinFeeCalculating;
    use futures::FutureExt;
    use num::BigRational;
    use shared::price_estimate::mocks::FakePriceEstimator;

    #[test]
    fn calculate_sell() {
        let mut fee_calculator = MockMinFeeCalculating::new();
        fee_calculator
            .expect_min_fee()
            .returning(|_, _, _, _| Ok((U256::from(3), Utc::now())));
        let price_estimator = FakePriceEstimator(BigRational::from_float(0.5).unwrap());
        let result = calculate(
            Arc::new(fee_calculator),
            Arc::new(price_estimator),
            Query {
                sell_token: H160::from_low_u64_ne(0),
                buy_token: H160::from_low_u64_ne(1),
                in_amount_before_fees: 10.into(),
                kind: OrderKind::Sell,
            },
        )
        .now_or_never()
        .unwrap()
        .unwrap();
        assert_eq!(result.min_fee, 3.into());
        assert_eq!(result.out_amount_before_fees, 20.into());
        // After the deducting the fee 10 - 3 = 7 units of sell token are being sold.
        assert_eq!(result.out_amount_after_fees, 14.into());
    }

    #[test]
    fn calculate_buy() {
        let mut fee_calculator = MockMinFeeCalculating::new();
        fee_calculator
            .expect_min_fee()
            .returning(|_, _, _, _| Ok((U256::from(3), Utc::now())));
        let price_estimator = FakePriceEstimator(BigRational::from_float(2.0).unwrap());
        let result = calculate(
            Arc::new(fee_calculator),
            Arc::new(price_estimator),
            Query {
                sell_token: H160::from_low_u64_ne(0),
                buy_token: H160::from_low_u64_ne(1),
                in_amount_before_fees: 10.into(),
                kind: OrderKind::Buy,
            },
        )
        .now_or_never()
        .unwrap()
        .unwrap();
        // To buy 10 units of buy_token the fee in sell_token must be at least 3 and at least 20
        // units of sell_token must be sold.
        assert_eq!(result.min_fee, 3.into());
        assert_eq!(result.out_amount_before_fees, 20.into());
        assert_eq!(result.out_amount_after_fees, 20.into());
    }
}
