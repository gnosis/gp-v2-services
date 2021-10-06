use crate::api::get_fee_and_quote::{FeeError, FeeParameters};
use crate::api::WarpReplyConverting;
use crate::{
    api::{
        self,
        order_validation::{OrderValidator, PreOrderData, ValidationError},
    },
    fee::MinFeeCalculating,
};
use anyhow::{anyhow, Result};
use ethcontract::{H160, U256};
use model::{
    app_id::AppId,
    order::{BuyTokenDestination, OrderKind, SellTokenSource},
    u256_decimal,
};
use serde::{Deserialize, Serialize};
use shared::price_estimation::{self, PriceEstimating, PriceEstimationError};
use std::{convert::Infallible, sync::Arc};
use warp::{
    hyper::StatusCode,
    reply::{self, Json},
    Filter, Rejection, Reply,
};

/// The order parameters to quote a price and fee for.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OrderQuoteRequest {
    from: H160,
    sell_token: H160,
    buy_token: H160,
    receiver: Option<H160>,
    #[serde(flatten)]
    side: OrderQuoteSide,
    valid_to: u32,
    app_data: AppId,
    partially_fillable: bool,
    #[serde(default)]
    sell_token_balance: SellTokenSource,
    #[serde(default)]
    buy_token_balance: BuyTokenDestination,
}

impl From<&OrderQuoteRequest> for PreOrderData {
    fn from(quote_request: &OrderQuoteRequest) -> Self {
        let owner = quote_request.from;
        Self {
            owner,
            sell_token: quote_request.sell_token,
            buy_token: quote_request.buy_token,
            receiver: quote_request.receiver.unwrap_or(owner),
            valid_to: quote_request.valid_to,
            buy_token_balance: quote_request.buy_token_balance,
            sell_token_balance: quote_request.sell_token_balance,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum OrderQuoteSide {
    #[serde(rename_all = "camelCase")]
    Sell {
        #[serde(flatten)]
        sell_amount: SellAmount,
    },
    #[serde(rename_all = "camelCase")]
    Buy {
        #[serde(with = "u256_decimal")]
        buy_amount_after_fee: U256,
    },
}

impl Default for OrderQuoteSide {
    fn default() -> Self {
        Self::Buy {
            buy_amount_after_fee: U256::one(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum SellAmount {
    BeforeFee {
        #[serde(rename = "sellAmountBeforeFee", with = "u256_decimal")]
        value: U256,
    },
    AfterFee {
        #[serde(rename = "sellAmountAfterFee", with = "u256_decimal")]
        value: U256,
    },
}

/// The quoted order by the service.
#[derive(Default, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderQuote {
    from: H160,
    sell_token: H160,
    buy_token: H160,
    receiver: Option<H160>,
    #[serde(with = "u256_decimal")]
    sell_amount: U256,
    #[serde(with = "u256_decimal")]
    buy_amount: U256,
    valid_to: u32,
    app_data: AppId,
    #[serde(with = "u256_decimal")]
    fee_amount: U256,
    kind: OrderKind,
    partially_fillable: bool,
    sell_token_balance: SellTokenSource,
    buy_token_balance: BuyTokenDestination,
}

#[derive(Debug)]
pub enum OrderQuoteError {
    Fee(FeeError),
    Order(ValidationError),
    // RateLimit, // TODO - use this.
}

impl OrderQuoteError {
    pub fn convert_to_reply(self) -> (Json, StatusCode) {
        match self {
            OrderQuoteError::Fee(err) => err.to_warp_reply(),
            OrderQuoteError::Order(err) => err.to_warp_reply(),
            // FeeAndQuoteError::RateLimit => (
            //     super::error("RateLimit", "Too many order quotes"),
            //     StatusCode::TOO_MANY_REQUESTS,
            // ),
        }
    }
}

impl OrderQuoteRequest {
    /// This method is used by the old, deprecated, fee endpoint to convert {Buy, Sell}Requests
    pub fn new(sell_token: H160, buy_token: H160, side: OrderQuoteSide) -> Self {
        Self {
            sell_token,
            buy_token,
            side,
            ..Default::default()
        }
    }

    async fn calculate_quote(
        &self,
        fee_calculator: Arc<dyn MinFeeCalculating>,
        price_estimator: Arc<dyn PriceEstimating>,
        order_validator: Arc<OrderValidator>,
    ) -> Result<OrderQuote, OrderQuoteError> {
        tracing::debug!("Received quote request {:?}", self);
        order_validator
            .partial_validate(self.into())
            .await
            .map_err(|err| OrderQuoteError::Order(ValidationError::Partial(err)))?;
        let fee_parameters = self
            .calculate_fee_parameters(fee_calculator, price_estimator)
            .await
            .map_err(OrderQuoteError::Fee)?;
        Ok(OrderQuote {
            from: self.from,
            sell_token: self.sell_token,
            buy_token: self.buy_token,
            receiver: self.receiver,
            sell_amount: fee_parameters.sell_amount,
            buy_amount: fee_parameters.buy_amount,
            valid_to: self.valid_to,
            app_data: self.app_data,
            fee_amount: fee_parameters.fee_amount,
            kind: fee_parameters.kind,
            partially_fillable: self.partially_fillable,
            sell_token_balance: self.sell_token_balance,
            buy_token_balance: self.buy_token_balance,
        })
    }

    pub async fn calculate_fee_parameters(
        &self,
        fee_calculator: Arc<dyn MinFeeCalculating>,
        price_estimator: Arc<dyn PriceEstimating>,
    ) -> Result<FeeParameters, FeeError> {
        match self.side {
            OrderQuoteSide::Sell {
                sell_amount:
                    SellAmount::BeforeFee {
                        value: sell_amount_before_fee,
                    },
            } => {
                if sell_amount_before_fee.is_zero() {
                    Err(FeeError::PriceEstimate(PriceEstimationError::ZeroAmount))
                } else {
                    let (fee, expiration) = fee_calculator
                        .min_fee(
                            self.sell_token,
                            Some(self.buy_token),
                            Some(sell_amount_before_fee),
                            Some(OrderKind::Sell),
                        )
                        .await
                        .map_err(FeeError::PriceEstimate)?;
                    let sell_amount_after_fee = sell_amount_before_fee
                        .checked_sub(fee)
                        .ok_or(FeeError::SellAmountDoesNotCoverFee)?
                        .max(U256::one());
                    let estimate = price_estimator
                        .estimate(&price_estimation::Query {
                            sell_token: self.sell_token,
                            buy_token: self.buy_token,
                            in_amount: sell_amount_after_fee,
                            kind: OrderKind::Sell,
                        })
                        .await
                        .map_err(FeeError::PriceEstimate)?;
                    Ok(FeeParameters {
                        buy_amount: estimate.out_amount,
                        sell_amount: sell_amount_before_fee,
                        fee_amount: fee,
                        expiration,
                        kind: OrderKind::Sell,
                    })
                }
            }
            OrderQuoteSide::Sell {
                sell_amount: SellAmount::AfterFee { .. },
            } => {
                // TODO: Nice to have: true sell amount after the fee (more complicated).
                Err(FeeError::PriceEstimate(PriceEstimationError::Other(
                    anyhow!("Currently unsupported route"),
                )))
            }
            OrderQuoteSide::Buy {
                buy_amount_after_fee,
            } => {
                if buy_amount_after_fee.is_zero() {
                    Err(FeeError::PriceEstimate(PriceEstimationError::ZeroAmount))
                } else {
                    let (fee, expiration) = fee_calculator
                        .min_fee(
                            self.sell_token,
                            Some(self.buy_token),
                            Some(buy_amount_after_fee),
                            Some(OrderKind::Buy),
                        )
                        .await
                        .map_err(FeeError::PriceEstimate)?;
                    let estimate = price_estimator
                        .estimate(&price_estimation::Query {
                            sell_token: self.sell_token,
                            buy_token: self.buy_token,
                            in_amount: buy_amount_after_fee,
                            kind: OrderKind::Buy,
                        })
                        .await
                        .map_err(FeeError::PriceEstimate)?;
                    let sell_amount_before_fee =
                        estimate.out_amount.checked_add(fee).ok_or_else(|| {
                            FeeError::PriceEstimate(PriceEstimationError::Other(anyhow!(
                                "overflow in sell_amount_before_fee"
                            )))
                        })?;
                    Ok(FeeParameters {
                        buy_amount: buy_amount_after_fee,
                        sell_amount: sell_amount_before_fee,
                        fee_amount: fee,
                        expiration,
                        kind: OrderKind::Buy,
                    })
                }
            }
        }
    }
}

fn post_quote_request() -> impl Filter<Extract = (OrderQuoteRequest,), Error = Rejection> + Clone {
    warp::path!("quote")
        .and(warp::post())
        .and(api::extract_payload())
}

fn post_quote_response(result: Result<OrderQuote, OrderQuoteError>) -> impl Reply {
    match result {
        Ok(quote) => reply::with_status(reply::json(&quote), StatusCode::OK),
        Err(err) => {
            let (reply, status) = err.convert_to_reply();
            reply::with_status(reply, status)
        }
    }
}

pub fn post_quote(
    fee_calculator: Arc<dyn MinFeeCalculating>,
    price_estimator: Arc<dyn PriceEstimating>,
    order_validator: Arc<OrderValidator>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    post_quote_request().and_then(move |request: OrderQuoteRequest| {
        let fee_calculator = fee_calculator.clone();
        let price_estimator = price_estimator.clone();
        let order_validator = order_validator.clone();
        async move {
            let result = request
                .calculate_quote(fee_calculator, price_estimator, order_validator)
                .await;
            if let Err(err) = &result {
                tracing::error!(?err, ?request, "post_quote error");
            }
            Result::<_, Infallible>::Ok(post_quote_response(result))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::response_body;
    use crate::fee::MockMinFeeCalculating;
    use chrono::{DateTime, NaiveDateTime, Utc};
    use futures::FutureExt;
    use serde_json::json;
    use shared::price_estimation::mocks::FakePriceEstimator;
    use warp::test::request;

    #[test]
    fn deserializes_sell_after_fees_quote_request() {
        assert_eq!(
            serde_json::from_value::<OrderQuoteRequest>(json!({
                "from": "0x0101010101010101010101010101010101010101",
                "sellToken": "0x0202020202020202020202020202020202020202",
                "buyToken": "0x0303030303030303030303030303030303030303",
                "kind": "sell",
                "sellAmountAfterFee": "1337",
                "validTo": 0x12345678,
                "appData": "0x9090909090909090909090909090909090909090909090909090909090909090",
                "partiallyFillable": false,
                "buyTokenBalance": "internal",
            }))
            .unwrap(),
            OrderQuoteRequest {
                from: H160([0x01; 20]),
                sell_token: H160([0x02; 20]),
                buy_token: H160([0x03; 20]),
                receiver: None,
                side: OrderQuoteSide::Sell {
                    sell_amount: SellAmount::AfterFee { value: 1337.into() },
                },
                valid_to: 0x12345678,
                app_data: AppId([0x90; 32]),
                partially_fillable: false,
                sell_token_balance: SellTokenSource::Erc20,
                buy_token_balance: BuyTokenDestination::Internal,
            }
        );
    }

    #[test]
    fn deserializes_sell_before_fees_quote_request() {
        assert_eq!(
            serde_json::from_value::<OrderQuoteRequest>(json!({
                "from": "0x0101010101010101010101010101010101010101",
                "sellToken": "0x0202020202020202020202020202020202020202",
                "buyToken": "0x0303030303030303030303030303030303030303",
                "kind": "sell",
                "sellAmountBeforeFee": "1337",
                "validTo": 0x12345678,
                "appData": "0x9090909090909090909090909090909090909090909090909090909090909090",
                "partiallyFillable": false,
                "sellTokenBalance": "external",
            }))
            .unwrap(),
            OrderQuoteRequest {
                from: H160([0x01; 20]),
                sell_token: H160([0x02; 20]),
                buy_token: H160([0x03; 20]),
                receiver: None,
                side: OrderQuoteSide::Sell {
                    sell_amount: SellAmount::BeforeFee { value: 1337.into() },
                },
                valid_to: 0x12345678,
                app_data: AppId([0x90; 32]),
                partially_fillable: false,
                sell_token_balance: SellTokenSource::External,
                buy_token_balance: BuyTokenDestination::Erc20,
            }
        );
    }

    #[test]
    fn deserializes_buy_quote_request() {
        assert_eq!(
            serde_json::from_value::<OrderQuoteRequest>(json!({
                "from": "0x0101010101010101010101010101010101010101",
                "sellToken": "0x0202020202020202020202020202020202020202",
                "buyToken": "0x0303030303030303030303030303030303030303",
                "receiver": "0x0404040404040404040404040404040404040404",
                "kind": "buy",
                "buyAmountAfterFee": "1337",
                "validTo": 0x12345678,
                "appData": "0x9090909090909090909090909090909090909090909090909090909090909090",
                "partiallyFillable": false,
            }))
            .unwrap(),
            OrderQuoteRequest {
                from: H160([0x01; 20]),
                sell_token: H160([0x02; 20]),
                buy_token: H160([0x03; 20]),
                receiver: Some(H160([0x04; 20])),
                side: OrderQuoteSide::Buy {
                    buy_amount_after_fee: U256::from(1337),
                },
                valid_to: 0x12345678,
                app_data: AppId([0x90; 32]),
                partially_fillable: false,
                sell_token_balance: SellTokenSource::Erc20,
                buy_token_balance: BuyTokenDestination::Erc20,
            }
        );
    }

    #[tokio::test]
    async fn post_quote_request_ok() {
        let filter = post_quote_request();
        let request_payload = OrderQuoteRequest::default();
        let request = request()
            .path("/quote")
            .method("POST")
            .header("content-type", "application/json")
            .json(&request_payload);
        let result = request.filter(&filter).await.unwrap();
        assert_eq!(result, request_payload);
    }

    #[tokio::test]
    async fn post_quote_request_err() {
        let filter = post_quote_request();
        let request_payload = OrderQuoteRequest::default();
        // Path is wrong!
        let request = request()
            .path("/fee_quote")
            .method("POST")
            .header("content-type", "application/json")
            .json(&request_payload);
        assert!(request.filter(&filter).await.is_err());
    }

    #[tokio::test]
    async fn post_quote_response_ok() {
        let response = post_quote_response(Ok(OrderQuote::default())).into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_body(response).await;
        let body: serde_json::Value = serde_json::from_slice(body.as_slice()).unwrap();
        let expected = serde_json::to_value(OrderQuote::default()).unwrap();
        assert_eq!(body, expected);
    }

    #[tokio::test]
    async fn post_quote_response_err() {
        let response = post_quote_response(Err(OrderQuoteError::Order(ValidationError::Other(
            anyhow!("Uh oh - error"),
        ))))
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response_body(response).await;
        let body: serde_json::Value = serde_json::from_slice(body.as_slice()).unwrap();
        let expected_error = json!({"errorType": "InternalServerError", "description": ""});
        assert_eq!(body, expected_error);
        // There are many other FeeAndQuoteErrors, but writing a test for each would follow the same pattern as this.
    }

    #[test]
    fn calculate_fee_sell_before_fees_quote_request() {
        let mut fee_calculator = MockMinFeeCalculating::new();

        let expiration = Utc::now();
        fee_calculator
            .expect_min_fee()
            .returning(move |_, _, _, _| Ok((U256::from(3), expiration)));

        let fee_calculator = Arc::new(fee_calculator);
        let price_estimator = FakePriceEstimator(price_estimation::Estimate {
            out_amount: 14.into(),
            gas: 1000.into(),
        });
        let sell_query = OrderQuoteRequest::new(
            H160::from_low_u64_ne(0),
            H160::from_low_u64_ne(1),
            OrderQuoteSide::Sell {
                sell_amount: SellAmount::BeforeFee { value: 10.into() },
            },
        );

        let result = sell_query
            .calculate_fee_parameters(fee_calculator, Arc::new(price_estimator))
            .now_or_never()
            .unwrap()
            .unwrap();
        // After the deducting the fee 10 - 3 = 7 units of sell token are being sold.
        assert_eq!(
            result,
            FeeParameters {
                buy_amount: 14.into(),
                sell_amount: 10.into(),
                fee_amount: 3.into(),
                expiration,
                kind: OrderKind::Sell
            }
        );
    }

    #[test]
    fn calculate_fee_sell_after_fees_quote_request() {
        let mut fee_calculator = MockMinFeeCalculating::new();
        fee_calculator
            .expect_min_fee()
            .returning(|_, _, _, _| Ok((U256::from(3), Utc::now())));

        let fee_calculator = Arc::new(fee_calculator);
        let price_estimator = FakePriceEstimator(price_estimation::Estimate {
            out_amount: 14.into(),
            gas: 1000.into(),
        });
        let sell_query = OrderQuoteRequest::new(
            H160::from_low_u64_ne(0),
            H160::from_low_u64_ne(1),
            OrderQuoteSide::Sell {
                sell_amount: SellAmount::AfterFee { value: 7.into() },
            },
        );

        let result = sell_query
            .calculate_fee_parameters(fee_calculator, Arc::new(price_estimator))
            .now_or_never()
            .unwrap()
            .unwrap_err();
        assert_eq!(
            format!("{:?}", result),
            "PriceEstimate(Other(Currently unsupported route))"
        );
    }

    #[test]
    fn calculate_fee_buy_quote_request() {
        let mut fee_calculator = MockMinFeeCalculating::new();
        let expiration = Utc::now();
        fee_calculator
            .expect_min_fee()
            .returning(move |_, _, _, _| Ok((U256::from(3), expiration)));

        let fee_calculator = Arc::new(fee_calculator);
        let price_estimator = FakePriceEstimator(price_estimation::Estimate {
            out_amount: 20.into(),
            gas: 1000.into(),
        });
        let buy_query = OrderQuoteRequest::new(
            H160::from_low_u64_ne(0),
            H160::from_low_u64_ne(1),
            OrderQuoteSide::Buy {
                buy_amount_after_fee: 10.into(),
            },
        );
        let result = buy_query
            .calculate_fee_parameters(fee_calculator, Arc::new(price_estimator))
            .now_or_never()
            .unwrap()
            .unwrap();
        // To buy 10 units of buy_token the fee in sell_token must be at least 3 and at least 20
        // units of sell_token must be sold.
        assert_eq!(
            result,
            FeeParameters {
                buy_amount: 10.into(),
                sell_amount: 23.into(),
                fee_amount: 3.into(),
                expiration,
                kind: OrderKind::Buy
            }
        );
    }

    #[test]
    fn pre_order_data_from_quote_request() {
        let quote_request = OrderQuoteRequest::default();
        let result = PreOrderData::from(&quote_request);
        let expected = PreOrderData::default();
        assert_eq!(result, expected);
    }

    // #[test]
    // fn calculate_quote() {
    //     let buy_request = OrderQuoteRequest {
    //         sell_token: H160::from_low_u64_be(1),
    //         buy_token: H160::from_low_u64_be(2),
    //         side: OrderQuoteSide::Buy {
    //             buy_amount_after_fee: 2.into(),
    //         },
    //         ..Default::default()
    //     };
    //
    //     let mut fee_calculator = MockMinFeeCalculating::new();
    //     fee_calculator
    //         .expect_min_fee()
    //         .returning(move |_, _, _, _| Ok((U256::from(3), Utc::now())));
    //     let fee_calculator = Arc::new(fee_calculator);
    //     let price_estimator = Arc::new(FakePriceEstimator(price_estimation::Estimate {
    //         out_amount: 14.into(),
    //         gas: 1000.into(),
    //     }));
    //
    //     // TODO - mock Order Validator

    //     let result = buy_request
    //         .build_quote_from_fee_response(fee_response)
    //         .unwrap();
    //     let expected = OrderQuote {
    //         sell_token: H160::from_low_u64_be(1),
    //         buy_token: H160::from_low_u64_be(2),
    //         sell_amount: 3.into(),
    //         kind: OrderKind::Buy,
    //         buy_amount: 2.into(),
    //         fee_amount: 1.into(),
    //         ..Default::default()
    //     };
    //     assert_eq!(result, expected);
    // }
}
