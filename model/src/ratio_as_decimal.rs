use bigdecimal::{BigDecimal, One};
use num::{bigint::Sign as Sign04, BigRational};
use num_bigint::{BigInt, Sign as Sign03};
use serde::{de, Deserialize, Deserializer, Serializer};
use serde_with::{DeserializeAs, SerializeAs};
use std::borrow::Cow;
use std::str::FromStr;

pub struct DecimalBigRational;

impl<'de> DeserializeAs<'de, BigRational> for DecimalBigRational {
    fn deserialize_as<D>(deserializer: D) -> Result<BigRational, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize(deserializer)
    }
}

impl<'de> SerializeAs<BigRational> for DecimalBigRational {
    fn serialize_as<S>(source: &BigRational, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize(source, serializer)
    }
}

pub fn serialize<S>(value: &BigRational, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let top_bytes = value.numer().to_bytes_le();
    let top = BigInt::from_bytes_le(sign_04_to_03(top_bytes.0), &top_bytes.1);
    let bottom_bytes = value.denom().to_bytes_le();
    let bottom = BigInt::from_bytes_le(sign_04_to_03(bottom_bytes.0), &bottom_bytes.1);
    let decimal = BigDecimal::from(top) / BigDecimal::from(bottom);
    serializer.serialize_str(&decimal.to_string())
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<BigRational, D::Error>
where
    D: Deserializer<'de>,
{
    let value_decimal: Result<BigDecimal, D::Error> =
        BigDecimal::from_str(&*Cow::<str>::deserialize(deserializer)?).map_err(|err| {
            de::Error::custom(format!("failed to decode decimal BigDecimal: {}", err))
        });

    match value_decimal {
        Ok(big_decimal) => {
            let (x, exp) = big_decimal.into_bigint_and_exponent();
            let numerator_bytes = x.to_bytes_le();
            let base = num::bigint::BigInt::from_bytes_le(
                sign_03_to_04(numerator_bytes.0),
                &numerator_bytes.1,
            );
            let ten = BigRational::new(10.into(), 1.into());
            let numerator = BigRational::new(base, num::bigint::BigInt::one());
            Ok(numerator / ten.pow(exp as i32))
        }
        Err(err) => Err(err),
    }
}

/// Simple one-to-one conversion of the Sign enum from num-bigint crates v0.3 and v0.4
fn sign_04_to_03(sign_04: Sign04) -> Sign03 {
    match sign_04 {
        Sign04::Minus => Sign03::Minus,
        Sign04::NoSign => Sign03::NoSign,
        Sign04::Plus => Sign03::Plus,
    }
}

fn sign_03_to_04(sign_03: Sign03) -> Sign04 {
    match sign_03 {
        Sign03::Minus => Sign04::Minus,
        Sign03::NoSign => Sign04::NoSign,
        Sign03::Plus => Sign04::Plus,
    }
}

#[cfg(test)]
mod tests {
    use crate::ratio_as_decimal::{deserialize, serialize};
    use num::{BigInt, BigRational, Zero};
    use serde_json::json;
    use serde_json::value::Serializer;

    #[test]
    fn serializer() {
        assert_eq!(
            serialize(&BigRational::from_float(1.2).unwrap(), Serializer).unwrap(),
            json!("1.1999999999999999555910790149937383830547332763671875")
        );
        assert_eq!(
            serialize(
                &BigRational::new(BigInt::from(1), BigInt::from(3)),
                Serializer
            )
            .unwrap(),
            json!("0.3333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333")
        );
        assert_eq!(
            serialize(&BigRational::zero(), Serializer).unwrap(),
            json!("0")
        );
        assert_eq!(
            serialize(
                &BigRational::new(BigInt::from(-1), BigInt::from(1)),
                Serializer
            )
            .unwrap(),
            json!("-1")
        );
    }

    #[test]
    fn deserialize_err() {
        assert!(deserialize(json!("hello")).is_err());
    }

    #[test]
    fn deserialize_ok() {
        assert_eq!(
            deserialize(json!("1.2")).unwrap(),
            BigRational::new(BigInt::from(12), BigInt::from(10))
        );
        assert_eq!(deserialize(json!("0")).unwrap(), BigRational::zero());
        assert_eq!(
            deserialize(json!("-1")).unwrap(),
            BigRational::new(BigInt::from(-1), BigInt::from(1))
        );
    }
}
