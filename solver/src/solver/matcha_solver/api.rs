//! Matcha HTTP API client implementation.
//!
//! For more information on the HTTP API, consult:
//! <https://0x.org/docs/api#request-1>
//! <https://api.0x.org/>

use crate::solver::solver_utils::{
    debug_bytes, deserialize_decimal_f64, deserialize_decimal_u256, Slippage,
};
use anyhow::Result;
use derivative::Derivative;
use ethcontract::{H160, U256};
use model::u256_decimal;
use reqwest::{Client, IntoUrl, Url};
use serde::Deserialize;
use shared::http::default_http_client;
use web3::types::Bytes;

/// A Matcha API quote query parameters.
///
/// These parameters are currently incomplete, and missing parameters can be
/// added incrementally as needed.
#[derive(Clone, Debug, Default)]
pub struct SwapQuery {
    /// Contract address of a token to sell.
    pub sell_token: H160,
    /// Contract address of a token to buy.
    pub buy_token: H160,
    /// Amount of a token to sell, set in atoms.
    pub sell_amount: Option<U256>,
    /// Amount of a token to sell, set in atoms.
    pub buy_amount: Option<U256>,
    /// Limit of price slippage you are willing to accept.
    pub slippage_percentage: Slippage,
    /// Flag to disable checks of the required quantities.
    pub skip_validation: Option<bool>,
}

impl SwapQuery {
    /// Encodes the swap query as
    fn into_url(self, base_url: &Url) -> Url {
        // The `Display` implementation for `H160` unfortunately does not print
        // the full address and instead uses ellipsis (e.g. "0xeeee…eeee"). This
        // helper just works around that.
        fn addr2str(addr: H160) -> String {
            format!("{:?}", addr)
        }

        let mut url = base_url
            .join("/swap/v1/quote?")
            .expect("unexpectedly invalid URL segment");
        url.query_pairs_mut()
            .append_pair("sellToken", &addr2str(self.sell_token))
            .append_pair("buyToken", &addr2str(self.buy_token))
            .append_pair("slippagePercentage", &self.slippage_percentage.to_string());
        // I am not setting any affiliate address, in order to save gas
        // .append_pair("affiliateAddress", "0xgp_address")
        if let Some(amount) = self.sell_amount {
            url.query_pairs_mut()
                .append_pair("sellAmount", &amount.to_string());
        }
        if let Some(amount) = self.buy_amount {
            url.query_pairs_mut()
                .append_pair("buyAmount", &amount.to_string());
        }
        if let Some(skip_validation) = self.skip_validation {
            url.query_pairs_mut()
                .append_pair("skipValidation", &skip_validation.to_string());
        }
        url
    }
}

/// A Matcha API swap response.
#[derive(Clone, Derivative, Deserialize, PartialEq)]
#[derivative(Debug)]
#[serde(rename_all = "camelCase")]
pub struct SwapResponse {
    #[serde(with = "u256_decimal")]
    pub sell_amount: U256,
    #[serde(with = "u256_decimal")]
    pub buy_amount: U256,
    pub allowance_target: H160,
    #[serde(deserialize_with = "deserialize_decimal_f64")]
    pub price: f64,
    pub to: H160,
    #[derivative(Debug(format_with = "debug_bytes"))]
    pub data: Bytes,
    #[serde(deserialize_with = "deserialize_decimal_u256")]
    pub value: U256,
}

/// Mockable implementation of the API for unit test
#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait MatchaApi {
    async fn get_swap(&self, query: SwapQuery) -> Result<SwapResponse>;
}

/// Matcha API Client implementation.
#[derive(Debug)]
pub struct DefaultMatchaApi {
    client: Client,
    base_url: Url,
}

impl DefaultMatchaApi {
    /// Create a new 1Inch HTTP API client with the specified base URL.
    pub fn new(base_url: impl IntoUrl) -> Result<Self> {
        Ok(Self {
            client: default_http_client()?,
            base_url: base_url.into_url()?,
        })
    }
}

#[async_trait::async_trait]
impl MatchaApi for DefaultMatchaApi {
    /// Retrieves a swap for the specified parameters from the 1Inch API.
    async fn get_swap(&self, query: SwapQuery) -> Result<SwapResponse> {
        Ok(self
            .client
            .get(query.into_url(&self.base_url))
            .send()
            .await?
            .json()
            .await?)
    }
}

impl Default for DefaultMatchaApi {
    fn default() -> Self {
        Self::new("https://api.0x.org/").expect("unexpected error parsing URL")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn test_api_e2e() {
        let matcha_client = DefaultMatchaApi::default();
        let sell_token = shared::addr!("EeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE");
        let buy_token = shared::addr!("1a5f9352af8af974bfc03399e3767df6370d82e4");
        let swap_query = SwapQuery {
            sell_token,
            buy_token,
            sell_amount: Some(1_000_000_000_000_000_000u128.into()),
            buy_amount: None,
            slippage_percentage: Slippage(0.1_f64),
            skip_validation: Some(true),
        };

        let price_response: SwapResponse = matcha_client.get_swap(swap_query).await.unwrap();

        println!("Price Response: {:?}", &price_response);
    }

    #[test]
    fn swap_query_serialization_0x_sell_order() {
        let base_url = Url::parse("https://api.0x.org/").unwrap();
        let url = SwapQuery {
            sell_token: shared::addr!("EeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE"),
            buy_token: shared::addr!("111111111117dc0aa78b770fa6a738034120c302"),
            sell_amount: Some(1_000_000_000_000_000_000u128.into()),
            buy_amount: None,
            slippage_percentage: Slippage::basis_points(30).unwrap(),
            skip_validation: None,
        }
        .into_url(&base_url);

        assert_eq!(
            url.as_str(),
            "https://api.0x.org/swap/v1/quote\
                    ?sellToken=0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\
                    &buyToken=0x111111111117dc0aa78b770fa6a738034120c302\
                    &slippagePercentage=0.3\
                    &sellAmount=1000000000000000000",
        );
    }

    #[test]
    fn swap_query_serialization_optional_skip_parameter() {
        let base_url = Url::parse("https://api.0x.org/").unwrap();
        let url = SwapQuery {
            sell_token: shared::addr!("EeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE"),
            buy_token: shared::addr!("111111111117dc0aa78b770fa6a738034120c302"),
            buy_amount: Some(1_000_000_000_000_000_000u128.into()),
            sell_amount: None,
            slippage_percentage: Slippage::basis_points(30).unwrap(),
            skip_validation: Some(true),
        }
        .into_url(&base_url);

        assert_eq!(
            url.as_str(),
            "https://api.0x.org/swap/v1/quote\
                    ?sellToken=0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\
                    &buyToken=0x111111111117dc0aa78b770fa6a738034120c302\
                    &slippagePercentage=0.3\
                    &buyAmount=1000000000000000000\
                    &skipValidation=true",
        );
    }

    #[test]
    fn deserialize_swap_response() {
        let swap = serde_json::from_str::<SwapResponse>(
                r#"{"price":"13.12100257517027783","guaranteedPrice":"12.98979254941857505","to":"0xdef1c0ded9bec7f1a1670819833240f027b25eff","data":"0xd9627aa40000000000000000000000000000000000000000000000000000000000000080000000000000000000000000000000000000000000000000016345785d8a00000000000000000000000000000000000000000000000000001206e6c0056936e100000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20000000000000000000000006810e776880c02933d47db1b9fc05908e5386b96869584cd0000000000000000000000001000000000000000000000000000000000000011000000000000000000000000000000000000000000000092415e982f60d431ba","value":"0","gas":"111000","estimatedGas":"111000","gasPrice":"10000000000","protocolFee":"0","minimumProtocolFee":"0","buyTokenAddress":"0x6810e776880c02933d47db1b9fc05908e5386b96","sellTokenAddress":"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2","buyAmount":"1312100257517027783","sellAmount":"100000000000000000","sources":[{"name":"0x","proportion":"0"},{"name":"Uniswap","proportion":"0"},{"name":"Uniswap_V2","proportion":"0"},{"name":"Eth2Dai","proportion":"0"},{"name":"Kyber","proportion":"0"},{"name":"Curve","proportion":"0"},{"name":"Balancer","proportion":"0"},{"name":"Balancer_V2","proportion":"0"},{"name":"Bancor","proportion":"0"},{"name":"mStable","proportion":"0"},{"name":"Mooniswap","proportion":"0"},{"name":"Swerve","proportion":"0"},{"name":"SnowSwap","proportion":"0"},{"name":"SushiSwap","proportion":"1"},{"name":"Shell","proportion":"0"},{"name":"MultiHop","proportion":"0"},{"name":"DODO","proportion":"0"},{"name":"DODO_V2","proportion":"0"},{"name":"CREAM","proportion":"0"},{"name":"LiquidityProvider","proportion":"0"},{"name":"CryptoCom","proportion":"0"},{"name":"Linkswap","proportion":"0"},{"name":"MakerPsm","proportion":"0"},{"name":"KyberDMM","proportion":"0"},{"name":"Smoothy","proportion":"0"},{"name":"Component","proportion":"0"},{"name":"Saddle","proportion":"0"},{"name":"xSigma","proportion":"0"},{"name":"Uniswap_V3","proportion":"0"},{"name":"Curve_V2","proportion":"0"}],"orders":[{"makerToken":"0x6810e776880c02933d47db1b9fc05908e5386b96","takerToken":"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2","makerAmount":"1312100257517027783","takerAmount":"100000000000000000","fillData":{"tokenAddressPath":["0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2","0x6810e776880c02933d47db1b9fc05908e5386b96"],"router":"0xd9e1ce17f2641f24ae83637ab66a2cca9c378b9f"},"source":"SushiSwap","sourcePathId":"0xf070a63548deb1c57a1540d63c986e01c1718a7a091d20da7020aa422c01b3de","type":0}],"allowanceTarget":"0xdef1c0ded9bec7f1a1670819833240f027b25eff","sellTokenToEthRate":"1","buyTokenToEthRate":"13.05137210499988309"}"#,
            )
            .unwrap();

        assert_eq!(
                swap,
                SwapResponse {
                    sell_amount: U256::from_dec_str("100000000000000000").unwrap(),
                     buy_amount: U256::from_dec_str("1312100257517027783").unwrap(),
                     allowance_target: shared::addr!("def1c0ded9bec7f1a1670819833240f027b25eff"),
                    price: 13.121_002_575_170_278_f64,
                    to: shared::addr!("def1c0ded9bec7f1a1670819833240f027b25eff"),
                    data: Bytes(hex::decode(
                        "d9627aa40000000000000000000000000000000000000000000000000000000000000080000000000000000000000000000000000000000000000000016345785d8a00000000000000000000000000000000000000000000000000001206e6c0056936e100000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc20000000000000000000000006810e776880c02933d47db1b9fc05908e5386b96869584cd0000000000000000000000001000000000000000000000000000000000000011000000000000000000000000000000000000000000000092415e982f60d431ba"
                    ).unwrap()),
                    value: U256::from_dec_str("0").unwrap(),
                }
            );
    }
}
