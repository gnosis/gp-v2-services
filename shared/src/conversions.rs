use anyhow::{anyhow, Result};
use num::BigInt;
use num::{bigint::Sign, Zero};
use num::{BigRational, ToPrimitive as _};
use primitive_types::U256;

pub fn big_rational_to_float(ratio: &BigRational) -> Option<f64> {
    Some(ratio.numer().to_f64()? / ratio.denom().to_f64()?)
}

pub fn big_rational_to_u256(ratio: &BigRational) -> Result<U256> {
    if ratio.denom() == &BigInt::zero() {
        return Err(anyhow!(" Division by 0 in BigRational to U256 conversion"));
    }
    big_int_to_u256(&(ratio.numer() / ratio.denom()))
}

pub fn u256_to_big_int(input: &U256) -> BigInt {
    let mut bytes = [0; 32];
    input.to_big_endian(&mut bytes);
    BigInt::from_bytes_be(Sign::Plus, &bytes)
}

pub fn u256_to_big_rational(input: &U256) -> BigRational {
    let as_bigint = u256_to_big_int(input);
    BigRational::new(as_bigint, 1.into())
}

pub fn big_int_to_u256(input: &BigInt) -> Result<U256> {
    let (sign, bytes) = input.to_bytes_be();
    if sign == Sign::Minus {
        return Err(anyhow!("Negative BigInt to U256 conversion"));
    }
    if bytes.len() > 32 {
        return Err(anyhow!("BigInt too big for U256 conversion"));
    }
    Ok(U256::from_big_endian(&bytes))
}

// Convenience:

pub trait U256Ext {
    fn to_big_int(&self) -> BigInt;
    fn to_big_rational(&self) -> BigRational;
}

impl U256Ext for U256 {
    fn to_big_int(&self) -> BigInt {
        u256_to_big_int(self)
    }
    fn to_big_rational(&self) -> BigRational {
        u256_to_big_rational(self)
    }
}
