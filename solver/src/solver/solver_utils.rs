use anyhow::{ensure, Result};
use contracts::{GPv2Settlement, ERC20};
use ethcontract::H160;
use ethcontract::U256;
use serde::{
    de::{Deserializer, Error as _},
    Deserialize,
};
use web3::types::Bytes;

use std::{
    borrow::Cow,
    fmt::{self, Display, Formatter},
};

// Helper trait to mock the smart contract interaction
#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait AllowanceFetching: Send + Sync {
    async fn existing_allowance(&self, token: H160, spender: H160) -> Result<U256>;
}

#[async_trait::async_trait]
impl AllowanceFetching for GPv2Settlement {
    async fn existing_allowance(&self, token: H160, spender: H160) -> Result<U256> {
        let token_contract = ERC20::at(&self.raw_instance().web3(), token);
        Ok(token_contract
            .allowance(self.address(), spender)
            .call()
            .await?)
    }
}

/// A slippage amount.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Default)]
pub struct Slippage(pub f64);

impl Slippage {
    /// Creates a slippage amount from the specified percentage.
    pub fn percentage(amount: f64) -> Result<Self> {
        // 1Inch API only accepts a slippage from 0 to 50.
        ensure!(
            (0. ..=50.).contains(&amount),
            "slippage outside of [0%, 50%] range"
        );
        Ok(Slippage(amount))
    }

    /// Creates a slippage amount from the specified basis points.
    pub fn basis_points(bps: u16) -> Result<Self> {
        let percent = (bps as f64) / 100.;
        Slippage::percentage(percent)
    }
}

impl Display for Slippage {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub fn deserialize_decimal_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let decimal_str = Cow::<str>::deserialize(deserializer)?;
    decimal_str.parse::<f64>().map_err(D::Error::custom)
}

pub fn deserialize_decimal_u256<'de, D>(deserializer: D) -> Result<U256, D::Error>
where
    D: Deserializer<'de>,
{
    let decimal_str = Cow::<str>::deserialize(deserializer)?;
    U256::from_dec_str(&*decimal_str).map_err(D::Error::custom)
}

pub fn deserialize_prefixed_hex<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    let prefixed_hex_str = Cow::<str>::deserialize(deserializer)?;
    let hex_str = prefixed_hex_str
        .strip_prefix("0x")
        .ok_or_else(|| D::Error::custom("hex missing '0x' prefix"))?;
    hex::decode(hex_str).map_err(D::Error::custom)
}

pub fn debug_bytes(
    bytes: &Bytes,
    formatter: &mut std::fmt::Formatter,
) -> Result<(), std::fmt::Error> {
    formatter.write_fmt(format_args!("0x{}", hex::encode(&bytes.0)))
}
