use super::H160Wrapper;
use crate::api::convert_get_trades_error_to_reply;
use crate::database::{Database, TradeFilter};
use crate::orderbook::Orderbook;
use anyhow::Result;
use futures::TryStreamExt;
use model::order::OrderUid;
use model::trade::Trade;
use serde::Deserialize;
use std::{convert::Infallible, sync::Arc};
use warp::{hyper::StatusCode, reply, Filter, Rejection, Reply};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Query {
    pub order_uid: Option<OrderUid>,
    pub owner: Option<H160Wrapper>,
}

impl Query {
    fn trade_filter(&self) -> TradeFilter {
        let to_h160 = |option: Option<&H160Wrapper>| option.map(|wrapper| wrapper.0);
        TradeFilter {
            order_uid: self.order_uid,
            owner: to_h160(self.owner.as_ref()),
        }
    }
}

pub fn get_trades_request() -> impl Filter<Extract = (TradeFilter,), Error = Rejection> + Clone {
    warp::path!("trades")
        .and(warp::get())
        .and(warp::query::<Query>())
        .map(|query: Query| query.trade_filter())
}

pub fn get_trades_response(result: Result<Vec<Trade>>) -> impl Reply {
    match result {
        Ok(trades) => Ok(reply::with_status(reply::json(&trades), StatusCode::OK)),
        Err(err) => Ok(convert_get_trades_error_to_reply(err)),
    }
}

pub fn get_trades(
    db: Database
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    get_trades_request().and_then(move |trade_filter| {
        async move {
            let result = get_trades_from_db(&db, &trade_filter).await;
            Result::<_, Infallible>::Ok(get_trades_response(result))
        }
    })
}

pub async fn get_trades_from_db(db: &Database, filter: &TradeFilter) -> Result<Vec<Trade>> {
    Ok(db.trades(filter).try_collect::<Vec<_>>().await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::response_body;
    use hex_literal::hex;
    use primitive_types::H160;
    use warp::test::{request, RequestBuilder};

    #[tokio::test]
    async fn get_trades_request_ok() {
        let trade_filter = |request: RequestBuilder| async move {
            let filter = get_trades_request();
            request.method("GET").filter(&filter).await
        };
        let result = trade_filter(request().path("/trades")).await.unwrap();
        assert_eq!(result.owner, None);
        assert_eq!(result.order_uid, None);

        let owner = H160::from_slice(&hex!("0000000000000000000000000000000000000001"));
        let mut uid = OrderUid([0u8; 56]);
        uid.0[0] = 0x01;
        uid.0[55] = 0xff;
        let path = format!("/trades?owner=0x{:x}&orderUid={:}", owner, uid);

        let request = request().path(path.as_str());
        let result = trade_filter(request).await.unwrap();
        assert_eq!(result.owner, Some(owner));
        assert_eq!(result.order_uid, Some(uid));
    }

    #[tokio::test]
    async fn get_orders_response_ok() {
        let trades = vec![Trade::default()];
        let response = get_trades_response(Ok(trades.clone())).into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_body(response).await;
        let response_trades: Vec<Trade> = serde_json::from_slice(body.as_slice()).unwrap();
        assert_eq!(response_trades, trades);
    }
