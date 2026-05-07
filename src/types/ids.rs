use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// MemoryId newtype
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct MemoryId(pub String);

impl MemoryId {
    pub fn new() -> Self {
        Self(format!("mem_{}", uuid::Uuid::new_v4().as_simple()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for MemoryId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MemoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for MemoryId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

// ---------------------------------------------------------------------------
// EntityId newtype
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct EntityId(pub String);

impl EntityId {
    pub fn new() -> Self {
        Self(format!("ent_{}", uuid::Uuid::new_v4().as_simple()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for EntityId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for EntityId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

// ---------------------------------------------------------------------------
// AgentId newtype
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct AgentId(pub String);

impl AgentId {
    pub fn new() -> Self {
        Self(format!("agent_{}", uuid::Uuid::new_v4().as_simple()))
    }

    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for AgentId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for AgentId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

// ---------------------------------------------------------------------------
// RequestId newtype (ADR-028)
//
// Stamped at MCP tool dispatch entry; threaded through tracing spans for
// cross-component correlation. UUIDv7 — time-ordered for log readability.
// Background tasks spawned post-create do NOT inherit this id; they use
// `operation_id = memory_id` for their own correlation.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub String);

impl RequestId {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

tokio::task_local! {
    /// Per-tool-call request id, set by `server::tool_call!` and read by
    /// downstream components (sidecar RPC, post-create extractors) to
    /// stamp the same id into outbound payloads.
    ///
    /// Background tasks spawned post-create do NOT inherit this — they use
    /// `operation_id = memory_id` (ADR-028).
    pub static REQUEST_ID: RequestId;
}

/// Read the current task-local `RequestId`, if any. Returns `None` when called
/// from a context that did not set it (e.g. background tasks, tests).
pub fn current_request_id() -> Option<RequestId> {
    REQUEST_ID.try_with(|r| r.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_id_has_correct_prefix() {
        let id = MemoryId::new();
        assert!(id.as_str().starts_with("mem_"), "got: {}", id);
    }

    #[test]
    fn memory_id_uniqueness() {
        let ids: Vec<MemoryId> = (0..100).map(|_| MemoryId::new()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len(), "generated duplicate MemoryIds");
    }

    #[test]
    fn memory_id_display_matches_inner() {
        let id = MemoryId::from("test_123".to_string());
        assert_eq!(format!("{id}"), "test_123");
        assert_eq!(id.as_str(), "test_123");
    }

    #[test]
    fn memory_id_serde_roundtrip() {
        let id = MemoryId::new();
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: MemoryId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, deserialized);
    }

    #[test]
    fn entity_id_has_correct_prefix() {
        let id = EntityId::new();
        assert!(id.as_str().starts_with("ent_"), "got: {}", id);
    }

    #[test]
    fn entity_id_serde_roundtrip() {
        let id = EntityId::new();
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: EntityId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, deserialized);
    }

    #[test]
    fn request_id_is_unique() {
        let a = RequestId::new();
        let b = RequestId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn request_id_display_matches_inner() {
        let id = RequestId(String::from("test_rid"));
        assert_eq!(format!("{id}"), "test_rid");
        assert_eq!(id.as_str(), "test_rid");
    }
}
