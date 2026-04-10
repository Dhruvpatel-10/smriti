//! Small serialization helpers for DB binding.

use crate::error::Result;
use serde::Serialize;

/// Convert a serializable enum to a `serde_json::Value` for SurrealDB parameter binding.
///
/// Replaces `serde_json::to_value(x).unwrap()` with proper error propagation.
pub fn enum_to_value<T: Serialize>(val: T) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(val)?)
}
