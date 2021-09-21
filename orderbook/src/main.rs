use contracts::{BalancerV2Vault, GPv2Settlement, WETH9};
use model::{
    order::{OrderUid, BUY_ETH_ADDRESS},
    DomainSeparator,
};
use orderbook::{
    account_balances::Web3BalanceFetcher,
    database::{self, orders::OrderFilter, Postgres},
    event_updater::EventUpdater,
    fee::EthAwareMinFeeCalculator,
    metrics::Metrics,
    orderbook::Orderbook,
    serve_task,
    solvable_orders::SolvableOrdersCache,
    verify_deployed_contract_constants,
};
use primitive_types::H160;
use shared::{
    bad_token::{
        cache::CachingDetector,
        list_based::{ListBasedDetector, UnknownTokenStrategy},
        trace_call::{
            AmmPairProviderFinder, BalancerVaultFinder, TokenOwnerFinding, TraceCallDetector,
        },
    },
    current_block::current_block_stream,
    maintenance::ServiceMaintenance,
    price_estimate::BaselinePriceEstimator,
    recent_block_cache::CacheConfig,
    sources::{
        self,
        uniswap::{
            pool_cache::PoolCache,
            pool_fetching::{PoolFetcher, PoolFetching},
        },
        PoolAggregator,
    },
    transport::create_instrumented_transport,
    transport::http::HttpTransport,
};
use shared::{baseline_solver::BaseTokens, metrics::setup_metrics_registry};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use structopt::StructOpt;
use tokio::task;
use url::Url;

#[derive(Debug, StructOpt)]
struct Arguments {
    #[structopt(flatten)]
    shared: shared::arguments::Arguments,

    #[structopt(long, env = "BIND_ADDRESS", default_value = "0.0.0.0:8080")]
    bind_address: SocketAddr,

    /// Url of the Postgres database. By default connects to locally running postgres.
    #[structopt(long, env = "DB_URL", default_value = "postgresql://")]
    db_url: Url,

    /// Skip syncing past events (useful for local deployments)
    #[structopt(long)]
    skip_event_sync: bool,

    /// The minimum amount of time an order has to be valid for.
    #[structopt(
        long,
        env = "MIN_ORDER_VALIDITY_PERIOD",
        default_value = "60",
        parse(try_from_str = shared::arguments::duration_from_seconds),
    )]
    min_order_validity_period: Duration,

    /// Don't use the trace_callMany api that only some nodes support to check whether a token
    /// should be denied.
    /// Note that if a node does not support the api we still use the less accurate call api.
    #[structopt(
        long,
        env = "SKIP_TRACE_API",
        parse(try_from_str),
        default_value = "false"
    )]
    skip_trace_api: bool,

    /// The amount of time a classification of a token into good or bad is valid for.
    #[structopt(
        long,
        env = "TOKEN_QUALITY_CACHE_EXPIRY_SECONDS",
        default_value = "600",
        parse(try_from_str = shared::arguments::duration_from_seconds),
    )]
    token_quality_cache_expiry: Duration,

    /// List of token addresses to be ignored throughout service
    #[structopt(long, env = "UNSUPPORTED_TOKENS", use_delimiter = true)]
    pub unsupported_tokens: Vec<H160>,

    /// List of account addresses to be denied from order creation
    #[structopt(long, env = "BANNED_USERS", use_delimiter = true)]
    pub banned_users: Vec<H160>,

    /// List of token addresses that should be allowed regardless of whether the bad token detector
    /// thinks they are bad. Base tokens are automatically allowed.
    #[structopt(long, env = "ALLOWED_TOKENS", use_delimiter = true)]
    pub allowed_tokens: Vec<H160>,

    /// The number of pairs that are automatically updated in the pool cache.
    #[structopt(long, env, default_value = "200")]
    pub pool_cache_lru_size: usize,

    /// Enable pre-sign orders. Pre-sign orders are accepted into the database without a valid
    /// signature, so this flag allows this feature to be turned off if malicious users are
    /// abusing the database by inserting a bunch of order rows that won't ever be valid.
    /// This flag can be removed once DDoS protection is implemented.
    #[structopt(long, env, parse(try_from_str), default_value = "false")]
    pub enable_presign_orders: bool,

    /// If solvable orders haven't been successfully update in this time attempting to get them
    /// errors and our liveness check fails.
    #[structopt(
        long,
        default_value = "300",
        parse(try_from_str = shared::arguments::duration_from_seconds),
    )]
    solvable_orders_max_update_age: Duration,
}

pub async fn database_metrics(metrics: Arc<Metrics>, database: Postgres) -> ! {
    loop {
        match database.count_rows_in_tables().await {
            Ok(counts) => {
                for (table, count) in counts {
                    metrics.set_table_row_count(table, count);
                }
            }
            Err(err) => tracing::error!(?err, "failed to update db metrics"),
        };
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

#[tokio::main]
async fn main() {
    let args = Arguments::from_args();
    shared::tracing::initialize(args.shared.log_filter.as_str());
    tracing::info!("running order book with validated {:#?}", args);

    setup_metrics_registry(Some("gp_v2_api".into()), None);
    let metrics = Arc::new(Metrics::new().unwrap());

    let client = shared::http_client(args.shared.http_timeout);

    let transport = create_instrumented_transport(
        HttpTransport::new(client.clone(), args.shared.node_url),
        metrics.clone(),
    );
    let web3 = web3::Web3::new(transport);
    let settlement_contract = GPv2Settlement::deployed(&web3)
        .await
        .expect("Couldn't load deployed settlement");
    let vault_relayer = settlement_contract
        .vault_relayer()
        .call()
        .await
        .expect("Couldn't get vault relayer address");
    let native_token = WETH9::deployed(&web3)
        .await
        .expect("couldn't load deployed native token");
    let chain_id = web3
        .eth()
        .chain_id()
        .await
        .expect("Could not get chainId")
        .as_u64();

    let network = web3
        .net()
        .version()
        .await
        .expect("Failed to retrieve network version ID");

    let native_token_price_estimation_amount = args
        .shared
        .amount_to_estimate_prices_with
        .or_else(|| shared::arguments::default_amount_to_estimate_prices_with(&network))
        .expect("No amount to estimate prices with set.");

    let vault = if BalancerV2Vault::raw_contract()
        .networks
        .contains_key(&network)
    {
        Some(
            BalancerV2Vault::deployed(&web3)
                .await
                .expect("couldn't load deployed vault contract"),
        )
    } else {
        // The Vault is not deployed on all networks we support, so allow it
        // to be `None` for those networks.
        tracing::warn!("No Balancer V2 Vault support for network {}", network);
        None
    };

    verify_deployed_contract_constants(&settlement_contract, chain_id)
        .await
        .expect("Deployed contract constants don't match the ones in this binary");
    let domain_separator = DomainSeparator::new(chain_id, settlement_contract.address());
    let postgres = Postgres::new(args.db_url.as_str()).expect("failed to create database");
    let database = Arc::new(database::instrumented::Instrumented::new(
        postgres.clone(),
        metrics.clone(),
    ));

    let sync_start = if args.skip_event_sync {
        web3.eth()
            .block_number()
            .await
            .map(|block| block.as_u64())
            .ok()
    } else {
        None
    };

    let event_updater = EventUpdater::new(
        settlement_contract.clone(),
        database.as_ref().clone(),
        sync_start,
    );
    let balance_fetcher = Arc::new(Web3BalanceFetcher::new(
        web3.clone(),
        vault,
        vault_relayer,
        settlement_contract.address(),
    ));

    let gas_price_estimator = Arc::new(
        shared::gas_price_estimation::create_priority_estimator(
            client.clone(),
            &web3,
            args.shared.gas_estimators.as_slice(),
        )
        .await
        .expect("failed to create gas price estimator"),
    );

    let pair_providers = sources::pair_providers(&args.shared.baseline_sources, chain_id, &web3)
        .await
        .values()
        .cloned()
        .collect::<Vec<_>>();

    let base_tokens = Arc::new(BaseTokens::new(
        native_token.address(),
        &args.shared.base_tokens,
    ));
    let mut allowed_tokens = args.allowed_tokens;
    allowed_tokens.extend(base_tokens.tokens().iter().copied());
    allowed_tokens.push(BUY_ETH_ADDRESS);
    let unsupported_tokens = args.unsupported_tokens;

    let mut finders: Vec<Arc<dyn TokenOwnerFinding>> = pair_providers
        .iter()
        .map(|provider| -> Arc<dyn TokenOwnerFinding> {
            Arc::new(AmmPairProviderFinder {
                inner: provider.clone(),
                base_tokens: base_tokens.tokens().iter().copied().collect(),
            })
        })
        .collect();
    if let Some(finder) = BalancerVaultFinder::new(&web3).await.unwrap() {
        finders.push(Arc::new(finder));
    }
    let trace_call_detector = TraceCallDetector {
        web3: web3.clone(),
        finders,
        base_tokens: base_tokens.tokens().clone(),
        settlement_contract: settlement_contract.address(),
    };
    let caching_detector = CachingDetector::new(
        Box::new(trace_call_detector),
        args.token_quality_cache_expiry,
    );
    let bad_token_detector = Arc::new(ListBasedDetector::new(
        allowed_tokens,
        unsupported_tokens,
        if args.skip_trace_api {
            UnknownTokenStrategy::Allow
        } else {
            UnknownTokenStrategy::Forward(Box::new(caching_detector))
        },
    ));

    let current_block_stream =
        current_block_stream(web3.clone(), args.shared.block_stream_poll_interval_seconds)
            .await
            .unwrap();

    let pool_aggregator = PoolAggregator {
        pool_fetchers: pair_providers
            .into_iter()
            .map(|pair_provider| {
                Arc::new(PoolFetcher {
                    pair_provider,
                    web3: web3.clone(),
                }) as Arc<dyn PoolFetching>
            })
            .collect(),
    };
    let pool_fetcher = Arc::new(
        PoolCache::new(
            CacheConfig {
                number_of_blocks_to_cache: args.shared.pool_cache_blocks,
                number_of_entries_to_auto_update: args.pool_cache_lru_size,
                maximum_recent_block_age: args.shared.pool_cache_maximum_recent_block_age,
                max_retries: args.shared.pool_cache_maximum_retries,
                delay_between_retries: args.shared.pool_cache_delay_between_retries_seconds,
            },
            Box::new(pool_aggregator),
            current_block_stream.clone(),
            metrics.clone(),
        )
        .expect("failed to create pool cache"),
    );

    let price_estimator = Arc::new(BaselinePriceEstimator::new(
        pool_fetcher.clone(),
        gas_price_estimator.clone(),
        base_tokens,
        bad_token_detector.clone(),
        native_token.address(),
        native_token_price_estimation_amount,
    ));
    let fee_calculator = Arc::new(EthAwareMinFeeCalculator::new(
        price_estimator.clone(),
        gas_price_estimator,
        native_token.address(),
        database.clone(),
        args.shared.fee_factor,
        bad_token_detector.clone(),
        native_token_price_estimation_amount,
    ));

    let solvable_orders_cache = SolvableOrdersCache::new(
        args.min_order_validity_period,
        database.clone(),
        balance_fetcher.clone(),
        bad_token_detector.clone(),
        current_block_stream.clone(),
    );
    let block = current_block_stream.borrow().number.unwrap().as_u64();
    solvable_orders_cache
        .update(block)
        .await
        .expect("failed to perform initial sovlable orders update");

    let orderbook = Arc::new(Orderbook::new(
        domain_separator,
        settlement_contract.address(),
        database.clone(),
        balance_fetcher,
        fee_calculator.clone(),
        args.min_order_validity_period,
        bad_token_detector,
        Box::new(web3.clone()),
        native_token.clone(),
        args.banned_users,
        args.enable_presign_orders,
        solvable_orders_cache,
        args.solvable_orders_max_update_age,
    ));
    let service_maintainer = ServiceMaintenance {
        maintainers: vec![database.clone(), Arc::new(event_updater), pool_fetcher],
    };
    check_database_connection(orderbook.as_ref()).await;

    let serve_task = serve_task(
        database.clone(),
        orderbook.clone(),
        fee_calculator,
        price_estimator,
        args.bind_address,
        metrics.clone(),
    );
    let maintenance_task =
        task::spawn(service_maintainer.run_maintenance_on_new_block(current_block_stream));
    let db_metrics_task = task::spawn(database_metrics(metrics, postgres));

    tokio::select! {
        result = serve_task => tracing::error!(?result, "serve task exited"),
        result = maintenance_task => tracing::error!(?result, "maintenance task exited"),
        result = db_metrics_task => tracing::error!(?result, "database metrics task exited"),
    };
}

async fn check_database_connection(orderbook: &Orderbook) {
    orderbook
        .get_orders(&OrderFilter {
            uid: Some(OrderUid::default()),
            ..Default::default()
        })
        .await
        .expect("failed to connect to database");
}
