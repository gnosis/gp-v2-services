pub mod api;
pub mod orderbook;

use orderbook::OrderBook;
use std::{net::SocketAddr, sync::Arc};
use tokio::{task, task::JoinHandle};
use warp::Filter;

pub fn serve_task(orderbook: Arc<OrderBook>) -> JoinHandle<()> {
    let filter = api::handle_all_routes(orderbook.clone())
        .map(|reply| warp::reply::with_header(reply, "Access-Control-Allow-Origin", "*"));
    let address = SocketAddr::new([0, 0, 0, 0].into(), 8080);
    tracing::info!(%address, "serving order book");
    task::spawn(warp::serve(filter).bind(address))
}
