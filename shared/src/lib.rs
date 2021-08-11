#[macro_use]
pub mod macros;

pub mod arguments;
pub mod bad_token;
pub mod baseline_solver;
pub mod conversions;
pub mod current_block;
pub mod ethcontract_error;
pub mod event_handling;
pub mod gas_price_estimation;
pub mod maintenance;
pub mod metrics;
pub mod network;
pub mod price_estimate;
pub mod recent_block_cache;
pub mod sources;
pub mod subgraph;
pub mod time;
pub mod token_info;
pub mod token_list;
pub mod trace_many;
pub mod tracing;
pub mod transport;
pub mod vault;
pub mod web3_traits;

use ethcontract::H160;
use hex::{FromHex, FromHexError};
use model::h160_hexadecimal;
use serde::Deserialize;
use std::{
    future::Future,
    str::FromStr,
    time::{Duration, Instant},
};

pub type Web3Transport = transport::instrumented::MetricTransport<transport::http::HttpTransport>;
pub type Web3 = web3::Web3<Web3Transport>;

/// Wraps H160 with FromStr and Deserialize that can handle a `0x` prefix.
#[derive(Deserialize)]
#[serde(transparent)]
pub struct H160Wrapper(#[serde(with = "h160_hexadecimal")] pub H160);
impl FromStr for H160Wrapper {
    type Err = FromHexError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        Ok(H160Wrapper(H160(FromHex::from_hex(s)?)))
    }
}

/// The standard http client we use in the api and driver.
pub fn http_client(timeout: Duration) -> reqwest::Client {
    reqwest::ClientBuilder::new()
        .timeout(timeout)
        .user_agent("gp-v2-services/2.0.0")
        .build()
        .unwrap()
}

/// Run a future and callback with the time the future took. The call back can for example log the
/// time.
pub async fn measure_time<T>(future: impl Future<Output = T>, timer: impl FnOnce(Duration)) -> T {
    let start = Instant::now();
    let result = future.await;
    timer(start.elapsed());
    result
}
