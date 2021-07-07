use anyhow::Result;
use prometheus::{Histogram, HistogramOpts, HistogramVec, IntCounter, IntGaugeVec, Opts, Registry};
use shared::sources::balancer::pool_cache::WeightedPoolCacheMetrics;
use shared::{
    sources::uniswap::pool_cache::PoolCacheMetrics, transport::instrumented::TransportMetrics,
};
use std::{
    convert::Infallible,
    sync::Arc,
    time::{Duration, Instant},
};
use warp::{reply::Response, Filter, Reply};

pub struct Metrics {
    /// Incoming API request metrics
    api_requests: HistogramVec,
    db_table_row_count: IntGaugeVec,
    /// Outgoing RPC request metrics
    rpc_requests: HistogramVec,
    pool_cache_hits: IntCounter,
    pool_cache_misses: IntCounter,
    database_queries: HistogramVec,
}

impl Metrics {
    pub fn new(registry: &Registry) -> Result<Self> {
        let opts = HistogramOpts::new(
            "gp_v2_api_requests",
            "API Request durations labelled by route and response status code",
        );
        let api_requests = HistogramVec::new(opts, &["response", "request_type"]).unwrap();
        registry.register(Box::new(api_requests.clone()))?;

        let db_table_row_count = IntGaugeVec::new(
            Opts::new("gp_v2_api_table_rows", "Number of rows in db tables."),
            &["table"],
        )?;
        registry.register(Box::new(db_table_row_count.clone()))?;

        let opts = HistogramOpts::new(
            "gp_v2_api_transport_requests",
            "RPC Request durations labelled by method",
        );
        let rpc_requests = HistogramVec::new(opts, &["method"]).unwrap();
        registry.register(Box::new(rpc_requests.clone()))?;

        let pool_cache_hits = IntCounter::new(
            "gp_v2_api_pool_cache_hits",
            "Number of cache hits in the pool fetcher cache.",
        )?;
        registry.register(Box::new(pool_cache_hits.clone()))?;

        let pool_cache_misses = IntCounter::new(
            "gp_v2_api_pool_cache_misses",
            "Number of cache misses in the pool fetcher cache.",
        )?;
        registry.register(Box::new(pool_cache_misses.clone()))?;

        let opts = HistogramOpts::new(
            "gp_v2_api_database_queries",
            "Sql queries to our postgresql database.",
        );
        let database_queries = HistogramVec::new(opts, &["type"]).unwrap();
        registry.register(Box::new(database_queries.clone()))?;

        Ok(Self {
            api_requests,
            db_table_row_count,
            rpc_requests,
            pool_cache_hits,
            pool_cache_misses,
            database_queries,
        })
    }

    pub fn set_table_row_count(&self, table: &str, count: i64) {
        self.db_table_row_count
            .with_label_values(&[table])
            .set(count);
    }
}

impl TransportMetrics for Metrics {
    fn report_query(&self, label: &str, elapsed: Duration) {
        self.rpc_requests
            .with_label_values(&[label])
            .observe(elapsed.as_secs_f64())
    }
}

impl PoolCacheMetrics for Metrics {
    fn pools_fetched(&self, cache_hits: usize, cache_misses: usize) {
        self.pool_cache_hits.inc_by(cache_hits as u64);
        self.pool_cache_misses.inc_by(cache_misses as u64);
    }
}

impl WeightedPoolCacheMetrics for Metrics {
    fn pools_fetched(&self, cache_hits: usize, cache_misses: usize) {
        self.pool_cache_hits.inc_by(cache_hits as u64);
        self.pool_cache_misses.inc_by(cache_misses as u64);
    }
}

impl crate::database::instrumented::Metrics for Metrics {
    fn database_query_histogram(&self, label: &str) -> Histogram {
        self.database_queries.with_label_values(&[label])
    }
}

// Response wrapper needed because we cannot inspect the reply's status code without consuming it
struct MetricsReply {
    response: Response,
}

impl Reply for MetricsReply {
    fn into_response(self) -> Response {
        self.response
    }
}

// Wrapper struct to annotate a reply with a handler label for logging purposes
pub struct LabelledReply {
    inner: Box<dyn Reply>,
    label: &'static str,
}

impl LabelledReply {
    pub fn new(inner: impl Reply + 'static, label: &'static str) -> Self {
        Self {
            inner: Box::new(inner),
            label,
        }
    }
}

impl Reply for LabelledReply {
    fn into_response(self) -> Response {
        self.inner.into_response()
    }
}

pub fn start_request() -> impl Filter<Extract = (Instant,), Error = Infallible> + Clone {
    warp::any().map(Instant::now)
}

pub fn end_request(metrics: Arc<Metrics>, timer: Instant, reply: LabelledReply) -> impl Reply {
    let LabelledReply { inner, label } = reply;
    let response = inner.into_response();
    let elapsed = timer.elapsed().as_secs_f64();
    metrics
        .api_requests
        .with_label_values(&[response.status().as_str(), label])
        .observe(elapsed);
    MetricsReply { response }
}
