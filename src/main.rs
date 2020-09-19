pub mod api;
pub mod models;
use crate::api::api::api_start;
use crate::models::orderbook::OrderBook;
use crate::models::tokenlist::TokenList;

fn main() {
    let orderbook = OrderBook::new();
    let tokenlist = TokenList::new();
    api_start(orderbook, tokenlist);
}
