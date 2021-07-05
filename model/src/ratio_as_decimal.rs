use num::{BigRational, ToPrimitive};
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
    let float_rep = value.numer().to_f64().unwrap() / value.denom().to_f64().unwrap();
    serializer.serialize_str(&float_rep.to_string())
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

#[cfg(test)]
mod tests {
    use crate::ratio_as_decimal::{deserialize, serialize};
    use num::{BigInt, BigRational};
    use serde_json::value::Serializer;
    use serde_json::Value;

    #[test]
    fn serializer() {
        assert_eq!(
            serialize(&BigRational::from_float(1.2).unwrap(), Serializer).unwrap(),
            Value::String("1.2".to_string())
        );
        assert_eq!(
            serialize(
                &BigRational::new(BigInt::from(1), BigInt::from(3)),
                Serializer
            )
            .unwrap(),
            Value::String("0.3333333333333333".to_string())
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
    }
}
