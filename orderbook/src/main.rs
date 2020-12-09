mod api;
mod orderbook;

use crate::orderbook::OrderBook;
use std::{net::SocketAddr, sync::Arc};
use warp::Filter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let orderbook = Arc::new(OrderBook::default());
    let filter = api::handle_all_routes(orderbook)
        .map(|reply| warp::reply::with_header(reply, "Access-Control-Allow-Origin", "*"));
    let address = SocketAddr::new([0, 0, 0, 0].into(), 8080);
    tracing::info!(%address, "serving order book");
    warp::serve(filter).bind(address).await;
    tracing::error!("warp exited");
}
