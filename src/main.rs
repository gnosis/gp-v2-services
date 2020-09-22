pub mod api;
pub mod batcher;
pub mod models;

use crate::api::api::api_start;
use crate::batcher::batcher::batch_process;
use crate::models::orderbook::OrderBook;
use crate::models::token_list::TokenList;
use std::time::Duration;
use tokio::join;
use tokio::spawn;
use tokio::time::delay_for;

#[tokio::main]
pub async fn main() {
    let orderbook = OrderBook::new();
    let token_list = TokenList::new();
    let orderbook_for_api = orderbook.clone();
    let token_list_for_api = token_list.clone();
    let handler_api = api_start(orderbook_for_api, token_list_for_api);
    let handler_driver = driver_start(orderbook, token_list);
    join!(handler_api, handler_driver);
}

async fn driver_start(orderbook: OrderBook, token_list: TokenList) {
    loop {
        let orderbook_for_iteration = orderbook.clone();
        let token_list_for_iteration = token_list.clone();
        spawn(
            async move { batch_process(orderbook_for_iteration, token_list_for_iteration).await },
        );
        delay_for(Duration::from_secs(5)).await;
    }
}
