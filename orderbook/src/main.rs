mod api;
mod orderbook;

use crate::orderbook::OrderBook;
use ethcontract::{Http, Web3};
use model::DomainSeparator;
use primitive_types::H160;
use std::fs;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use structopt::StructOpt;
use tokio::task;
use warp::Filter;

#[path = "../../contracts/src/paths.rs"]
mod paths;

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
    // Todo: merge valentins PR to get it from env
    const NODE_URL: &str = "http://localhost:8545";

    let http = Http::new(NODE_URL).expect("Node url invalid, or service not available");
    let web3 = Web3::new(http);

    let chain_id = web3.eth().chain_id().await.expect("Could not get chainId");
    let address_file = paths::contract_address_file("GPv2Settlement");
    let contract_address: H160 = fs::read_to_string(&address_file)
        .expect("Could not retrieve Settlement Contract address")[2..]
        .parse()
        .expect("Could not parse Settlement Contract address");
    let domain_separator =
        DomainSeparator::get_domain_separator(chain_id.as_u64(), contract_address);
    let args = Arguments::from_args();
    tracing_setup::initialize(args.shared.log_filter.as_str());
    tracing::info!("running order book with {:#?}", args);
    let orderbook = Arc::new(OrderBook::new(domain_separator));
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
