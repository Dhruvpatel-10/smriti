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
}
