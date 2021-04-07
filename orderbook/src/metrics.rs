use std::{convert::Infallible, sync::Arc, time::Instant};

use prometheus::{HistogramOpts, HistogramVec, Registry};
use warp::Filter;
use warp::{reply::Response, Reply};

pub struct Metrics {
    requests: HistogramVec,
}

impl Metrics {
    pub fn new(registry: &Registry) -> Self {
        let opts = HistogramOpts::new(
            "gp_v2_api_requests",
            "API Request durations labelled by route and response status code",
        );
        let requests = HistogramVec::new(opts, &["response", "request_type"]).unwrap();
        registry
            .register(Box::new(requests.clone()))
            .expect("Failed to register metric");
        Self { requests }
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

pub fn start_request() -> impl Filter<Extract = (Instant,), Error = Infallible> + Clone {
    warp::any().map(Instant::now)
}

pub fn end_request(metrics: Arc<Metrics>, timer: Instant, reply: impl Reply) -> impl Reply {
    let response = reply.into_response();
    let elapsed = timer.elapsed().as_secs_f64();
    metrics
        .requests
        .with_label_values(&[response.status().as_str(), "TODO"])
        .observe(elapsed);
    MetricsReply { response }
}
