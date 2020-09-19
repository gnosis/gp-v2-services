pub mod api;
pub mod models;
use crate::api::api::api_start;
use crate::models::orderbook::OrderBook;

fn main() {
    let orderbook = OrderBook::new();

    api_start(orderbook);
}
