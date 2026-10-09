//! Attribute keys and values, as OpenTelemetry's data model defines them.

use std::borrow::Cow;

/// An attribute key. Field names from `tracing` are `'static`, so most keys
/// cost nothing.
pub type Key = Cow<'static, str>;

/// An attribute value: OpenTelemetry's `AnyValue`.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A boolean.
    Bool(bool),
    /// A signed 64-bit integer.
    I64(i64),
    /// A double.
    F64(f64),
    /// A string.
    String(String),
    /// Raw bytes.
    Bytes(Vec<u8>),
    /// A list of values.
    Array(Vec<Value>),
    /// A list of key/value pairs.
    Map(Vec<KeyValue>),
}

/// A key and its value.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyValue {
    /// The key.
    pub key: Key,
    /// The value.
    pub value: Value,
}

impl KeyValue {
    /// A key/value pair.
    pub fn new(key: impl Into<Key>, value: impl Into<Value>) -> Self {
        KeyValue {
            key: key.into(),
            value: value.into(),
        }
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::I64(v)
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::I64(v.into())
    }
}

impl From<u32> for Value {
    fn from(v: u32) -> Self {
        Value::I64(v.into())
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::F64(v)
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::String(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::String(v.to_string())
    }
}

impl From<Vec<Value>> for Value {
    fn from(v: Vec<Value>) -> Self {
        Value::Array(v)
    }
}

/// Set `key` in `attributes`, replacing an existing value: attributes are a
/// map, and a later `record` overrides an earlier one.
pub(crate) fn upsert(attributes: &mut Vec<KeyValue>, kv: KeyValue) -> bool {
    match attributes.iter_mut().find(|a| a.key == kv.key) {
        Some(existing) => {
            existing.value = kv.value;
            true
        }
        None => {
            attributes.push(kv);
            false
        }
    }
}

/// An integer that doesn't fit OpenTelemetry's i64 becomes a string.
pub(crate) fn int_value(fits: Option<i64>, value: impl ToString) -> Value {
    match fits {
        Some(v) => Value::I64(v),
        None => Value::String(value.to_string()),
    }
}
