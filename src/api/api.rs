use super::filter::get;
use super::filter::post_order;

use crate::models::OrderBook;
use core::future::Future;
use ethcontract::web3::types::{Address, H256, U256};
use serde::{Deserialize, Serialize};
use warp::Filter;

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize, Default)]
pub struct SignedOrder {
    pub sell_amount: U256,
    pub buy_amount: U256,
    pub buy_token: Address,
    pub sell_token: Address,
    pub owner: Address,
    pub nonce: u8,
    pub signature_v: u8,
    pub signature_r: H256,
    pub signature_s: H256,
    pub valid_until: U256,
}

impl SignedOrder {
    #[cfg(test)]
    pub fn new_valid_test_signed_order() -> Self {
        SignedOrder {
            sell_amount: U256::from_dec_str("1000000000000000000").unwrap(),
            buy_amount: U256::from_dec_str("900000000000000000").unwrap(),
            sell_token: "A193E42526F1FEA8C99AF609dcEabf30C1c29fAA".parse().unwrap(),
            buy_token: "FDFEF9D10d929cB3905C71400ce6be1990EA0F34".parse().unwrap(),
            owner: "63FC2aD3d021a4D7e64323529a55a9442C444dA0".parse().unwrap(),
            nonce: 1,
            signature_v: 27 as u8,
            signature_r: "07cf23fa6f588cc3a91de8444b589e5afbf91c5d486c512a353d45d02fa58700"
                .parse()
                .unwrap(),
            signature_s: "53671e75b62b5bd64f91c80430aafb002040c35d1fcf25d0dc55d978946d5c11"
                .parse()
                .unwrap(),
            valid_until: U256::from("0"),
        }
    }
}

pub fn run_api(orderbook: OrderBook) -> impl Future<Output = ()> + 'static {
    let routes = post_order(orderbook.clone()).or(get(orderbook.clone()));
    warp::serve(routes).bind(([127, 0, 0, 1], 3030))
}

#[cfg(test)]
pub mod test_util {
    use super::*;
    use crate::models::Order;
    use warp::http::StatusCode;
    use warp::test::request;

    #[tokio::test]
    async fn test_rending_of_get_request() {
        let orderbook = OrderBook::new();
        let order = Order::new_valid_test_order();
        let orderbook_api = orderbook.clone();
        orderbook.add_order(order.clone()).await;
        let filter = get(orderbook_api.clone());

        let result = request()
            .path("/v1/orders")
            .method("GET")
            .reply(&filter)
            .await;
        let result_orderbook: OrderBook = serde_json::from_slice(result.body()).unwrap();

        assert!(orderbook
            .orders
            .read()
            .await
            .eq(&result_orderbook.orders.read().await.clone()));
    }
    #[tokio::test]
    async fn test_post_new_valid_order() {
        let orderbook = OrderBook::new();
        let filter = post_order(orderbook.clone());

        let resp = request()
            .path("/v1/orders")
            .method("POST")
            .header("content-type", "application/json")
            .json(&SignedOrder::new_valid_test_signed_order())
            .reply(&filter)
            .await;

        assert_eq!(resp.status(), StatusCode::CREATED);
    }
    #[tokio::test]
    async fn test_post_new_invalid_order() {
        let orderbook = OrderBook::new();
        let filter = post_order(orderbook.clone());
        let mut order = SignedOrder::new_valid_test_signed_order();
        order.sell_amount = order.sell_amount + U256::one();
        let resp = request()
            .path("/v1/orders")
            .method("POST")
            .header("content-type", "application/json")
            .json(&order)
            .reply(&filter)
            .await;

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
    #[tokio::test]
    async fn test_post_two_times_valid_order() {
        let orderbook = OrderBook::new();
        let filter = post_order(orderbook.clone());
        let order = SignedOrder::new_valid_test_signed_order();
        warp::test::request()
            .path("/v1/orders")
            .method("POST")
            .header("content-type", "application/json")
            .json(&order)
            .reply(&filter)
            .await;
        let resp = request()
            .path("/v1/orders")
            .method("POST")
            .header("content-type", "application/json")
            .json(&order)
            .reply(&filter)
            .await;

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
