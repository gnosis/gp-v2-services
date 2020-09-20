pub mod api;
pub mod batcher;
pub mod models;

use crate::api::api::api_start;
use crate::batcher::batcher::batch_process;
use crate::models::orderbook::OrderBook;
use crate::models::token_list::TokenList;
use std::thread;
use std::time::Duration;

#[tokio::main]
pub async fn main() {
    let orderbook = OrderBook::new();
    let token_list = TokenList::new();
    let orderbook_for_api = orderbook.clone();
    let token_list_for_api = token_list.clone();
    thread::spawn(move || async { api_start(orderbook_for_api, token_list_for_api) });
    loop {
        let orderbook_for_iteration = orderbook.clone();
        let token_list_for_iteration = token_list.clone();
        thread::spawn(move || {
            batch_process(
                orderbook_for_iteration.clone(),
                token_list_for_iteration.clone(),
            )
        });
        thread::sleep(Duration::from_secs(1));
    }
}
