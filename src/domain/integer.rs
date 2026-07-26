use std::fmt;

use serde::{
    Deserializer, Serializer,
    de::{Error, Visitor},
};

pub(crate) mod i128_string {
    use super::*;

    pub fn serialize<S>(value: &i128, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<i128, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(I128Visitor)
    }

    struct I128Visitor;

    impl Visitor<'_> for I128Visitor {
        type Value = i128;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a signed 128-bit integer encoded as a string or integer")
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
            Ok(i128::from(value))
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
            Ok(i128::from(value))
        }

        fn visit_i128<E>(self, value: i128) -> Result<Self::Value, E> {
            Ok(value)
        }

        fn visit_u128<E>(self, value: u128) -> Result<Self::Value, E>
        where
            E: Error,
        {
            i128::try_from(value).map_err(E::custom)
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: Error,
        {
            value.parse().map_err(E::custom)
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: Error,
        {
            self.visit_str(&value)
        }
    }
}

pub(crate) mod option_i128_string {
    use super::*;

    pub fn serialize<S>(value: &Option<i128>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) => serializer.serialize_some(&value.to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<i128>, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_option(OptionI128Visitor)
    }

    struct OptionI128Visitor;

    impl<'de> Visitor<'de> for OptionI128Visitor {
        type Value = Option<i128>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an optional signed 128-bit integer")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            i128_string::deserialize(deserializer).map(Some)
        }
    }
}
