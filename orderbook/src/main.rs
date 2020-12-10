mod api;
mod orderbook;

use crate::orderbook::OrderBook;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use structopt::StructOpt;
use tokio::task;
use warp::Filter;

#[derive(Debug, StructOpt)]
struct Arguments {
    #[structopt(flatten)]
    shared: shared_arguments::Arguments,

    #[structopt(long, env = "BIND_ADDRESS", default_value = "0.0.0.0:8080")]
    bind_address: SocketAddr,
}

const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(10);

pub async fn orderbook_maintenance(orderbook: Arc<OrderBook>) -> ! {
    loop {
        tracing::debug!("running order book maintenance");
        orderbook.run_maintenance().await;
        tokio::time::delay_for(MAINTENANCE_INTERVAL).await;
    }
}
#[tokio::main]
async fn main() {
    let args = Arguments::from_args();
    tracing_setup::initialize(args.shared.log_filter.as_str());
    tracing::info!("running order book with {:#?}", args);
    let orderbook = Arc::new(OrderBook::new(args.shared.domain_separator));
    let filter = api::handle_all_routes(orderbook.clone())
        .map(|reply| warp::reply::with_header(reply, "Access-Control-Allow-Origin", "*"));
    let address = SocketAddr::new([0, 0, 0, 0].into(), 8080);
    tracing::info!(%address, "serving order book");
    let serve_task = task::spawn(warp::serve(filter).bind(address));
    let maintenance_task = task::spawn(orderbook_maintenance(orderbook));
    tokio::select! {
        result = serve_task => tracing::error!(?result, "serve task exited"),
        result = maintenance_task => tracing::error!(?result, "maintenance task exited"),
    };
}
