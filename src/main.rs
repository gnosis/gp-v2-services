mod api;
mod batcher;
mod models;
mod solver;
use crate::api::api::run_api;
use crate::batcher::batcher::batch_process;
use crate::models::OrderBook;
use std::time::Duration;
use tokio::time::delay_for;
use tokio::{select, spawn};

#[tokio::main]
async fn main() {
    let orderbook = OrderBook::new();
    let handler_api = run_api(orderbook.clone());
    let handler_driver = run_driver(orderbook);

    select! {
        e = handler_api => {
            println!("run_api returned  {:?}", e);
        },
        e = handler_driver => {
            println!("handler_driver returned {:?}", e);
        }
    }
}

async fn run_driver(orderbook: OrderBook) {
    loop {
        let orderbook_for_iteration = orderbook.clone();
        spawn(async move {
            let res = batch_process(orderbook_for_iteration)
                .await
                .map_err(|e| format!(" {:?} while async call batch_process", e));
            match res {
                Err(e) => println!("{:}", e),
                Ok(_) => (),
            };
        });
        delay_for(Duration::from_secs(1)).await;
    }
}
