pub mod api;
pub mod batcher;
pub mod models;

use crate::api::api::run_api;
use crate::batcher::batcher::batch_process;
use crate::models::orderbook::OrderBook;
use crate::models::token_list::TokenList;
use std::time::Duration;
use tokio::time::delay_for;
use tokio::{select, spawn};

#[tokio::main]
pub async fn main() {
    let orderbook = OrderBook::new();
    let token_list = TokenList::new();
    let orderbook_for_api = orderbook.clone();
    let token_list_for_api = token_list.clone();
    let handler_api = run_api(orderbook_for_api, token_list_for_api);
    let handler_driver = run_driver(orderbook, token_list);
    select! {
        err = handler_api => {
            println!("run_api returned the following error {:?}", err);
        }
        err = handler_driver => {
            println!("run_driver returned the following error {:?}", err);
        }
    }
}

async fn run_driver(orderbook: OrderBook, token_list: TokenList) {
    loop {
        let orderbook_for_iteration = orderbook.clone();
        let token_list_for_iteration = token_list.clone();
        spawn(async move {
            batch_process(orderbook_for_iteration, token_list_for_iteration)
                .await
                .unwrap();
        });
        delay_for(Duration::from_secs(15)).await;
    }
}
