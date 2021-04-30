use contracts::{IUniswapLikeRouter, WETH9};
use ethcontract::{Account, PrivateKey, H160};
use prometheus::Registry;
use reqwest::Url;
use shared::{amm_pair_provider::SushiswapPairProvider, network::network_name};
use shared::{
    amm_pair_provider::UniswapPairProvider,
    metrics::serve_metrics,
    pool_fetching::PoolFetcher,
    price_estimate::UniswapPriceEstimator,
    token_info::{CachedTokenInfoFetcher, TokenInfoFetcher},
    transport::LoggingTransport,
};
use solver::{
    driver::Driver,
    liquidity::uniswap::UniswapLikeLiquidity,
    liquidity_collector::LiquidityCollector,
    metrics::Metrics,
    solver::{AmmSources, SolverType},
};
use std::iter::FromIterator as _;
use std::{collections::HashSet, sync::Arc, time::Duration};
use structopt::StructOpt;
use web3::transports::Http;

#[derive(Debug, StructOpt)]
struct Arguments {
    #[structopt(flatten)]
    shared: shared::arguments::Arguments,

    /// The API endpoint to fetch the orderbook
    #[structopt(long, env = "ORDERBOOK_URL", default_value = "http://localhost:8080")]
    orderbook_url: Url,

    /// The API endpoint to call the mip solver
    #[structopt(long, env = "MIP_SOLVER_URL", default_value = "http://localhost:8000")]
    mip_solver_url: Url,

    /// The timeout for the API endpoint to fetch the orderbook
    #[structopt(
        long,
        env = "ORDERBOOK_TIMEOUT",
        default_value = "10",
        parse(try_from_str = shared::arguments::duration_from_seconds),
    )]
    orderbook_timeout: Duration,

    /// The private key used by the driver to sign transactions.
    #[structopt(short = "k", long, env = "PRIVATE_KEY", hide_env_values = true)]
    private_key: PrivateKey,

    /// The target confirmation time for settlement transactions used to estimate gas price.
    #[structopt(
        long,
        env = "TARGET_CONFIRM_TIME",
        default_value = "30",
        parse(try_from_str = shared::arguments::duration_from_seconds),
    )]
    target_confirm_time: Duration,

    /// Every how often we should execute the driver's run loop
    #[structopt(
        long,
        env = "SETTLE_INTERVAL",
        default_value = "10",
        parse(try_from_str = shared::arguments::duration_from_seconds),
    )]
    settle_interval: Duration,

    /// Which type of solver to use
    #[structopt(
        long,
        env = "SOLVER_TYPE",
        default_value = "Naive,UniswapBaseline",
        possible_values = &SolverType::variants(),
        case_insensitive = true,
        use_delimiter = true,
    )]
    solvers: Vec<SolverType>,

    /// A settlement must contain at least one order older than this duration for it to be applied.
    /// Larger values delay individual settlements more but have a higher coincidence of wants
    /// chance.
    #[structopt(
        long,
        env = "MIN_ORDER_AGE",
        default_value = "30",
        parse(try_from_str = shared::arguments::duration_from_seconds),
    )]
    min_order_age: Duration,

    /// The port at which we serve our metrics
    #[structopt(
        long,
        env = "METRICS_PORT",
        default_value = "9587",
        case_insensitive = true
    )]
    metrics_port: u16,

    /// Which AMM sources to use. Multiple sources are supported alone or simultaneously
    #[structopt(
    long,
        env = "AMM_SOURCES",
        default_value = "Uniswap,Sushiswap",
        possible_values = &AmmSources::variants(),
        case_insensitive = true,
        use_delimiter = true
    )]
    pub amm_sources: Vec<AmmSources>,
}

#[tokio::main]
async fn main() {
    let args = Arguments::from_args();
    shared::tracing::initialize(args.shared.log_filter.as_str());
    tracing::info!("running solver with {:#?}", args);

    let registry = Registry::default();
    let metrics = Arc::new(Metrics::new(&registry).expect("Couldn't register metrics"));

    // TODO: custom transport that allows setting timeout
    let transport = LoggingTransport::new(
        web3::transports::Http::new(args.shared.node_url.as_str())
            .expect("transport creation failed"),
    );
    let web3 = web3::Web3::new(transport);
    let chain_id = web3
        .eth()
        .chain_id()
        .await
        .expect("Could not get chainId")
        .as_u64();
    let network_id = web3
        .net()
        .version()
        .await
        .expect("failed to get network id");
    let network_name = network_name(&network_id, chain_id);
    let account = Account::Offline(args.private_key, Some(chain_id));
    let settlement_contract = solver::get_settlement_contract(&web3, account)
        .await
        .expect("couldn't load deployed settlement");
    let native_token_contract = WETH9::deployed(&web3)
        .await
        .expect("couldn't load deployed native token");
    let orderbook_api = solver::orderbook::OrderBookApi::new(
        args.orderbook_url,
        args.orderbook_timeout,
        native_token_contract.clone(),
    );
    let mut base_tokens = HashSet::from_iter(args.shared.base_tokens);
    // We should always use the native token as a base token.
    base_tokens.insert(native_token_contract.address());

    let token_info_fetcher = Arc::new(CachedTokenInfoFetcher::new(Box::new(TokenInfoFetcher {
        web3: web3.clone(),
    })));
    let gas_price_estimator = shared::gas_price_estimation::create_priority_estimator(
        &reqwest::Client::new(),
        &web3,
        args.shared.gas_estimators.as_slice(),
    )
    .await
    .expect("failed to create gas price estimator");

    // TODO - Currently we use only Uniswap for price estimation.
    let price_estimator = Arc::new(UniswapPriceEstimator::new(
        Box::new(PoolFetcher {
            pair_provider: Arc::new(UniswapPairProvider {
                factory: contracts::UniswapV2Factory::deployed(&web3)
                    .await
                    .expect("couldn't load deployed uniswap router"),
                chain_id,
            }),
            web3: web3.clone(),
        }),
        base_tokens.clone(),
    ));
    let uniswap_like_liquidity = build_amm_artifacts(
        args.amm_sources,
        chain_id,
        settlement_contract.clone(),
        base_tokens.clone(),
        web3.clone(),
    )
    .await;
    let solver = solver::solver::create(
        args.solvers,
        base_tokens,
        native_token_contract.address(),
        args.mip_solver_url,
        token_info_fetcher,
        price_estimator.clone(),
        network_name.to_string(),
    );
    let liquidity_collector = LiquidityCollector {
        uniswap_like_liquidity,
        orderbook_api,
    };
    let mut driver = Driver::new(
        settlement_contract,
        liquidity_collector,
        price_estimator,
        solver,
        Box::new(gas_price_estimator),
        args.target_confirm_time,
        args.settle_interval,
        native_token_contract.address(),
        args.min_order_age,
        metrics,
        web3,
        network_id,
    );

    serve_metrics(registry, ([0, 0, 0, 0], args.metrics_port).into());
    driver.run_forever().await;
}

async fn build_amm_artifacts(
    sources: Vec<AmmSources>,
    chain_id: u64,
    settlement_contract: contracts::GPv2Settlement,
    base_tokens: HashSet<H160>,
    web3: web3::Web3<LoggingTransport<Http>>,
) -> Vec<UniswapLikeLiquidity> {
    let mut res = vec![];
    for source in sources {
        match source {
            AmmSources::Uniswap => {
                let router = contracts::UniswapV2Router02::deployed(&web3)
                    .await
                    .expect("couldn't load deployed uniswap router");
                let factory = contracts::UniswapV2Factory::deployed(&web3)
                    .await
                    .expect("couldn't load deployed uniswap router");
                let pair_provider = Arc::new(UniswapPairProvider { factory, chain_id });
                res.push(UniswapLikeLiquidity::new(
                    IUniswapLikeRouter::at(&web3, router.address()),
                    pair_provider.clone(),
                    settlement_contract.clone(),
                    base_tokens.clone(),
                    web3.clone(),
                ));
            }
            AmmSources::Sushiswap => {
                let router = contracts::SushiswapV2Router02::deployed(&web3)
                    .await
                    .expect("couldn't load deployed sushiswap router");
                let factory = contracts::SushiswapV2Factory::deployed(&web3)
                    .await
                    .expect("couldn't load deployed sushiswap router");
                let pair_provider = Arc::new(SushiswapPairProvider { factory });
                res.push(UniswapLikeLiquidity::new(
                    IUniswapLikeRouter::at(&web3, router.address()),
                    pair_provider.clone(),
                    settlement_contract.clone(),
                    base_tokens.clone(),
                    web3.clone(),
                ));
            }
        }
    }
    res
}
