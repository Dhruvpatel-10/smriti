//! Small serialization helpers for DB binding.

use crate::error::Result;
use serde::Serialize;

/// Convert a serializable enum to a `serde_json::Value` for SurrealDB parameter binding.
///
/// Replaces `serde_json::to_value(x).unwrap()` with proper error propagation.
pub fn enum_to_value<T: Serialize>(val: T) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(val)?)
}

/// Convert an optional serializable enum to a `serde_json::Value`.
/// `None` maps to `serde_json::Value::Null` (bound as NONE in SurrealDB).
pub fn opt_enum_to_value<T: Serialize>(val: Option<T>) -> Result<serde_json::Value> {
    match val {
        Some(t) => enum_to_value(t),
        None => Ok(serde_json::Value::Null),
    }
}

/// Strip a SurrealDB record-id payload to its bare string id.
///
/// SurrealDB returns record references in several shapes — `"table:id"` strings,
/// `{ "tb": …, "id": "…" }` objects, or `{ "tb": …, "id": { "String": "…" } }`
/// nested objects. This helper handles all three. Returns `None` for null /
/// missing / unparsable shapes.
pub fn record_id_bare(value: &serde_json::Value, table_prefix: &str) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => {
            let prefix = format!("{table_prefix}:");
            Some(s.strip_prefix(&prefix).unwrap_or(s).to_string())
        }
        serde_json::Value::Object(obj) => obj.get("id").and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(inner) => inner
                .get("String")
                .and_then(|s| s.as_str().map(String::from)),
            _ => None,
        }),
        _ => None,
    }
}
