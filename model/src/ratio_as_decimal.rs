use bigdecimal::BigDecimal;
use num::bigint::Sign as Sign04;
use num::BigRational;
use num_bigint::{BigInt, Sign as Sign03};
use serde::{de, Deserializer, Serializer};
use serde_with::{DeserializeAs, SerializeAs};
use std::fmt;
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
    struct Visitor {}
    impl<'de> de::Visitor<'de> for Visitor {
        type Value = BigRational;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            write!(
                formatter,
                "a BigRational encoded as a decimal encoded string"
            )
        }

        fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            BigRational::from_str(&s).map_err(|err| {
                de::Error::custom(format!(
                    "failed to decode {:?} as decimal BigRational: {}",
                    s, err
                ))
            })
        }
    }

    deserializer.deserialize_str(Visitor {})
}

/// Simple one-to-one conversion of the Sign enum from num-bigint crates v0.3 and v0.4
fn sign_04_to_03(sign_04: Sign04) -> Sign03 {
    match sign_04 {
        Sign04::Minus => Sign03::Minus,
        Sign04::NoSign => Sign03::NoSign,
        Sign04::Plus => Sign03::Plus,
    }
}

#[cfg(test)]
mod tests {
    use crate::ratio_as_decimal::{deserialize, serialize};
    use num::{BigInt, BigRational, Zero};
    use serde_json::value::Serializer;
    use serde_json::Value;

    #[test]
    fn serializer() {
        assert_eq!(
            serialize(&BigRational::from_float(1.2).unwrap(), Serializer).unwrap(),
            Value::String("1.1999999999999999555910790149937383830547332763671875".to_string())
        );
        assert_eq!(
            serialize(
                &BigRational::new(BigInt::from(1), BigInt::from(3)),
                Serializer
            )
            .unwrap(),
            Value::String("0.3333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333".to_string())
        );
        assert_eq!(
            serialize(&BigRational::zero(), Serializer).unwrap(),
            Value::String("0".to_string())
        );
    }

    #[test]
    fn deserialize_err() {
        let value = Value::String("hello".to_string());
        assert!(deserialize(value).is_err());
    }

    #[test]
    fn deserialize_ok() {
        assert_eq!(
            deserialize(Value::String("1/2".to_string())).unwrap(),
            BigRational::from_float(0.5).unwrap()
        );

        assert_eq!(
            deserialize(Value::String("0/1".to_string())).unwrap(),
            BigRational::zero()
        );
    }
}
