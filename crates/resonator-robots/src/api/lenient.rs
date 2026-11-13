//! Deserialization helpers that accept what Gson accepts: numbers for
//! strings, numeric strings for numbers, and `null` for a field that has a
//! default.

use std::collections::BTreeMap;

use serde::de::{Deserializer, Error};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON primitive as a string, the way `JsonPrimitive.getAsString` reads
/// it.
pub(crate) fn value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// A JSON primitive as an integer, the way Gson's integer adapters read it:
/// integral numbers and numeric strings.
pub(crate) fn value_as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| {
            let f = n.as_f64()?;
            (f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64).then_some(f as i64)
        }),
        Value::String(s) => s.trim().parse::<i64>().ok().or_else(|| {
            let f = s.trim().parse::<f64>().ok()?;
            (f.fract() == 0.0 && f.is_finite()).then_some(f as i64)
        }),
        _ => None,
    }
}

pub(crate) fn value_as_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        // Gson parses any string: "true" (ignoring case) or false.
        Value::String(s) => Some(s.eq_ignore_ascii_case("true")),
        _ => None,
    }
}

/// `null` or a string-like primitive.
pub(crate) fn opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let value = Value::deserialize(d)?;
    if value.is_null() {
        return Ok(None);
    }
    value_as_string(&value)
        .map(Some)
        .ok_or_else(|| D::Error::custom("expected a string"))
}

pub(crate) fn int64<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let value = Value::deserialize(d)?;
    value_as_i64(&value).ok_or_else(|| D::Error::custom("expected an integer"))
}

pub(crate) fn int32<'de, D: Deserializer<'de>>(d: D) -> Result<i32, D::Error> {
    let value = int64(d)?;
    i32::try_from(value).map_err(|_| D::Error::custom("integer out of range"))
}

pub(crate) fn boolean<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let value = Value::deserialize(d)?;
    value_as_bool(&value).ok_or_else(|| D::Error::custom("expected a boolean"))
}

/// A list of strings; `null` reads as empty.
pub(crate) fn string_list<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let values = Option::<Vec<Value>>::deserialize(d)?.unwrap_or_default();
    values
        .iter()
        .map(|v| value_as_string(v).ok_or_else(|| D::Error::custom("expected a string")))
        .collect()
}

/// A list; `null` reads as empty.
pub(crate) fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// `Map<String, String>` whose values may be `null`.
pub(crate) fn opt_string_map<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, Option<String>>, D::Error> {
    let map = Option::<BTreeMap<String, Value>>::deserialize(d)?.unwrap_or_default();
    map.into_iter()
        .map(|(key, value)| {
            if value.is_null() {
                return Ok((key, None));
            }
            value_as_string(&value)
                .map(|v| (key, Some(v)))
                .ok_or_else(|| D::Error::custom("expected a string"))
        })
        .collect()
}

/// `Map<String, String>` without `null` values (they are dropped); `null`
/// reads as empty.
pub(crate) fn string_map<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    Ok(opt_string_map(d)?
        .into_iter()
        .filter_map(|(k, v)| Some((k, v?)))
        .collect())
}

/// A `byte[]` as Gson's `ByteArrayGsonAdaptor` writes it: standard base64.
pub mod base64_bytes {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn encode(bytes: &[u8]) -> String {
        STANDARD.encode(bytes)
    }

    /// Decodes like commons-codec: both alphabets, padding optional,
    /// characters outside the alphabet skipped.
    pub fn decode(text: &str) -> Vec<u8> {
        let cleaned: String = text
            .chars()
            .filter_map(|c| match c {
                '-' => Some('+'),
                '_' => Some('/'),
                c if c.is_ascii_alphanumeric() || c == '+' || c == '/' => Some(c),
                _ => None,
            })
            .collect();
        let mut cleaned = cleaned;
        // A dangling single character carries no byte.
        if cleaned.len() % 4 == 1 {
            cleaned.pop();
        }
        base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(cleaned.as_bytes())
            .or_else(|_| {
                base64::engine::GeneralPurpose::new(
                    &base64::alphabet::STANDARD,
                    base64::engine::GeneralPurposeConfig::new()
                        .with_decode_allow_trailing_bits(true)
                        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
                )
                .decode(cleaned.as_bytes())
            })
            .unwrap_or_default()
    }

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = Option::<String>::deserialize(d)?
            .ok_or_else(|| D::Error::custom("expected a base64 string"))?;
        Ok(decode(&text))
    }
}

/// A list of `byte[]`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ByteArrays(pub Vec<Vec<u8>>);

impl Serialize for ByteArrays {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.0.iter().map(|bytes| base64_bytes::encode(bytes)))
    }
}

impl<'de> Deserialize<'de> for ByteArrays {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let items = Vec::<String>::deserialize(d)?;
        Ok(ByteArrays(
            items.iter().map(|s| base64_bytes::decode(s)).collect(),
        ))
    }
}
