use ethcontract::PrivateKey;
use reqwest::Url;
use solver::driver::Driver;
use std::time::Duration;
use structopt::StructOpt;
#[derive(Debug, StructOpt)]
struct Arguments {
    #[structopt(flatten)]
    shared: shared_arguments::Arguments,

    /// The API endpoint to fetch the orderbook
    #[structopt(long, env = "ORDERBOOK_URL", default_value = "http://localhost:8080")]
    orderbook_url: Url,

    /// The timeout for the API endpoint to fetch the orderbook
    #[structopt(
        long,
        env = "ORDERBOOK_TIMEOUT",
        default_value = "10",
        parse(try_from_str = shared_arguments::duration_from_seconds),
    )]
    orderbook_timeout: Duration,

    /// The private key used by the driver to sign transactions.
    #[structopt(short = "k", long, env = "PRIVATE_KEY", hide_env_values = true)]
    private_key: PrivateKey,
}

#[tokio::main]
async fn main() {
    let args = Arguments::from_args();
    tracing_setup::initialize(args.shared.log_filter.as_str());
    tracing::info!("running solver with {:#?}", args);
    // TODO: custom transport that allows setting timeout
    let transport = web3::transports::Http::new(args.shared.node_url.as_str())
        .expect("transport creation failed");
    let web3 = web3::Web3::new(transport);
    let uniswap_contract = contracts::UniswapV2Router02::deployed(&web3)
        .await
        .expect("couldn't load deployed uniswap router");
    let settlement_contract = solver::get_settlement_contract(&web3, 4, args.private_key)
        .await
        .expect("couldn't load deployed settlement");
    let orderbook =
        solver::orderbook::OrderBookApi::new(args.orderbook_url, args.orderbook_timeout);
    let mut driver = Driver::new(settlement_contract, uniswap_contract, orderbook);
    driver.run_forever().await;
}
