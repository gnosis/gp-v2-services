//! Module emulating the operations on fixed points with exactly 18 decimals as
//! used in the Balancer smart contracts. Their original implementation can be
//! found at:
//! https://github.com/balancer-labs/balancer-v2-monorepo/blob/6c9e24e22d0c46cca6dd15861d3d33da61a60b98/pkg/solidity-utils/contracts/math/FixedPoint.sol

#![allow(dead_code)]

use super::error::Error;
use anyhow::{anyhow, bail};
use ethcontract::U256;
use lazy_static::lazy_static;
use std::{
    fmt::{self, Debug, Formatter},
    str::FromStr,
};

mod logexpmath;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
/// Fixed point numbers that represent exactly any rational number that can be
/// represented with up to 18 decimals as long as it can be stored in 256 bits.
/// It corresponds to Solidity's `ufixed256x18`.
/// Operations on this type are implemented as in Balancer's FixedPoint library,
/// including error codes, from which the name (Balancer Fixed Point).
pub struct Bfp(U256);

lazy_static! {
    static ref ONE_18: U256 = U256::exp10(18);
    static ref ZERO: Bfp = Bfp(U256::zero());
    static ref EPSILON: Bfp = Bfp(U256::one());
    static ref ONE: Bfp = Bfp(*ONE_18);
    static ref MAX_POW_RELATIVE_ERROR: Bfp = Bfp(10000_usize.into());
}

impl From<usize> for Bfp {
    fn from(num: usize) -> Self {
        Self(U256::from(num).checked_mul(*ONE_18).unwrap())
    }
}

impl FromStr for Bfp {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut split_dot = s.splitn(2, '.');
        let units = split_dot.next().unwrap();
        let decimals = split_dot.next().unwrap_or("0");
        if units.is_empty() || decimals.is_empty() || decimals.len() > 18 {
            bail!("Invalid decimal representation");
        }
        Ok(Bfp(U256::from_dec_str(&format!("{:0<18}", decimals))?
            .checked_add(
                U256::from_dec_str(units)?
                    .checked_mul(*ONE_18)
                    .ok_or_else(|| anyhow!("Too large number"))?,
            )
            .ok_or_else(|| anyhow!("Too large number"))?))
    }
}

impl Debug for Bfp {
    fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
        write!(
            formatter,
            "{}.{:0>18}",
            self.0 / *ONE_18,
            (self.0 % *ONE_18).as_u128()
        )
    }
}

impl Bfp {
    pub const MAX: Self = Self(U256::MAX);

    pub fn as_uint256(self) -> U256 {
        self.0
    }

    pub fn zero() -> Self {
        *ZERO
    }

    pub fn one() -> Self {
        *ONE
    }

    pub fn epsilon() -> Self {
        *EPSILON
    }

    pub fn from_wei(num: U256) -> Self {
        Self(num)
    }

    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    pub fn add(self, other: Self) -> Result<Self, Error> {
        Ok(Self(self.0.checked_add(other.0).ok_or(Error::AddOverflow)?))
    }

    pub fn sub(self, other: Self) -> Result<Self, Error> {
        Ok(Self(self.0.checked_sub(other.0).ok_or(Error::SubOverflow)?))
    }

    pub fn mul_down(self, other: Self) -> Result<Self, Error> {
        Ok(Self(
            self.0.checked_mul(other.0).ok_or(Error::MulOverflow)? / *ONE_18,
        ))
    }

    pub fn mul_up(self, other: Self) -> Result<Self, Error> {
        let product = self.0.checked_mul(other.0).ok_or(Error::MulOverflow)?;

        Ok(if product.is_zero() {
            Bfp::zero()
        } else {
            Bfp(((product - 1) / *ONE_18) + 1)
        })
    }

    pub fn div_down(self, other: Self) -> Result<Self, Error> {
        if other.is_zero() {
            Err(Error::ZeroDivision)
        } else {
            Ok(Self(
                self.0.checked_mul(*ONE_18).ok_or(Error::DivInternal)? / other.0,
            ))
        }
    }

    pub fn div_up(self, other: Self) -> Result<Self, Error> {
        if other.is_zero() {
            return Err(Error::ZeroDivision);
        }
        if self.is_zero() {
            Ok(Self::zero())
        } else {
            let a_inflated = self.0.checked_mul(*ONE_18).ok_or(Error::DivInternal)?;

            Ok(Self(((a_inflated - 1) / other.0) + 1))
        }
    }

    pub fn complement(self) -> Self {
        if self.0 < *ONE_18 {
            Self(*ONE_18 - self.0)
        } else {
            Self::zero()
        }
    }

    pub fn pow_up(self, exp: Self) -> Result<Self, Error> {
        let raw = Bfp(logexpmath::pow(self.0, exp.0)?);
        let max_error = raw.mul_up(*MAX_POW_RELATIVE_ERROR)?.add(Bfp(1.into()))?;

        raw.add(max_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        assert_eq!("1".parse::<Bfp>().unwrap(), Bfp::one());
        assert_eq!(
            "0.1".parse::<Bfp>().unwrap(),
            Bfp::from_wei(U256::exp10(17))
        );
        assert_eq!(
            "1.01".parse::<Bfp>().unwrap(),
            Bfp::from_wei(U256::exp10(18) + U256::exp10(16))
        );
        assert_eq!(
            "10.000000000000000001".parse::<Bfp>().unwrap(),
            Bfp::from_wei(U256::exp10(19) + U256::one())
        );
        assert!("10.0000000000000000001".parse::<Bfp>().is_err());
        assert!("1.0.1".parse::<Bfp>().is_err());
        assert!(".1".parse::<Bfp>().is_err());
        assert!("1.".parse::<Bfp>().is_err());
        assert!("".parse::<Bfp>().is_err());
    }

    #[test]
    fn add() {
        assert_eq!(Bfp::from(40).add(2.into()).unwrap(), 42.into());

        assert_eq!(
            Bfp::MAX.add(Bfp::epsilon()).unwrap_err(),
            Error::AddOverflow
        );
    }

    #[test]
    fn sub() {
        assert_eq!(Bfp::from(50).sub(8.into()).unwrap(), 42.into());

        assert_eq!(
            Bfp::one().sub(Bfp(*ONE_18 + 1)).unwrap_err(),
            Error::SubOverflow
        );
    }

    macro_rules! test_mul {
        ($fn_name:ident) => {
            assert_eq!(Bfp::from(6).$fn_name(7.into()).unwrap(), 42.into());
            assert_eq!(Bfp::zero().$fn_name(Bfp::one()).unwrap(), Bfp::zero());
            assert_eq!(Bfp::one().$fn_name(Bfp::zero()).unwrap(), Bfp::zero());

            assert_eq!(
                Bfp::one()
                    .$fn_name(Bfp(U256::MAX / U256::exp10(18) + 1))
                    .unwrap_err(),
                Error::MulOverflow,
            );
        };
    }

    #[test]
    fn mul() {
        test_mul!(mul_down);
        test_mul!(mul_up);

        let one_half = Bfp((5 * 10_u128.pow(17)).into());
        assert_eq!(Bfp::epsilon().mul_down(one_half).unwrap(), Bfp::zero());
        assert_eq!(Bfp::epsilon().mul_up(one_half).unwrap(), Bfp::epsilon());
    }

    macro_rules! test_div {
        ($fn_name:ident) => {
            assert_eq!(Bfp::from(42).div_down(7.into()).unwrap(), 6.into());
            assert_eq!(Bfp::zero().div_down(Bfp::one()).unwrap(), 0.into());

            assert_eq!(
                Bfp::one().$fn_name(Bfp::zero()).unwrap_err(),
                Error::ZeroDivision
            );
            assert_eq!(
                Bfp(U256::MAX / U256::exp10(18) + 1)
                    .$fn_name(Bfp::one())
                    .unwrap_err(),
                Error::DivInternal,
            );
        };
    }

    #[test]
    fn div() {
        test_div!(div_down);
        test_div!(div_up);

        assert_eq!(Bfp::epsilon().div_down(2.into()).unwrap(), Bfp::zero());
        assert_eq!(Bfp::epsilon().div_up(2.into()).unwrap(), Bfp::epsilon());
    }

    #[test]
    fn pow_up() {
        assert_eq!(
            Bfp::from(2).pow_up(3.into()).unwrap(),
            Bfp(U256::from(8_000_000_000_000_079_990_u128))
        ); // powDown: 7999999999999919988
        assert_eq!(
            Bfp::from(2).pow_up(0.into()).unwrap(),
            Bfp(U256::from(1_000_000_000_000_010_001_u128))
        ); // powDown: 999999999999989999
        assert_eq!(
            Bfp::zero().pow_up(Bfp::one()).unwrap(),
            Bfp(U256::from(1_u128))
        ); // powDown: 0

        assert_eq!(
            Bfp::MAX.pow_up(Bfp::one()).unwrap_err(),
            Error::XOutOfBounds,
        );
        // note: the values were chosen to get a large value from `pow`
        assert_eq!(
            Bfp(U256::from_dec_str(
                "287200000000000000000000000000000000000000000000000000000000000000000000000"
            )
            .unwrap())
            .pow_up(Bfp::one())
            .unwrap_err(),
            Error::MulOverflow,
        );
    }

    #[test]
    fn complement() {
        assert_eq!(Bfp::zero().complement(), Bfp::one());
        assert_eq!(
            "0.424242424242424242".parse::<Bfp>().unwrap().complement(),
            "0.575757575757575758".parse().unwrap()
        );
        assert_eq!(Bfp::one().complement(), Bfp::zero());
        assert_eq!(
            "1.000000000000000001".parse::<Bfp>().unwrap().complement(),
            Bfp::zero()
        );
    }
}
