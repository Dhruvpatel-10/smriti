use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::enums::EntityKind;
use super::ids::EntityId;
use super::memory::MemorySummary;

/// Full entity record as stored in DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: EntityId,
    pub name: String,
    pub kind: EntityKind,
    pub roles: Vec<String>,
    pub attrs: serde_json::Value,
    pub context: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Summary returned by entity search.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntitySummary {
    pub id: EntityId,
    pub name: String,
    pub kind: EntityKind,
    pub roles: Vec<String>,
    pub context: Option<String>,
    pub memory_count: u64,
}

/// Full entity returned by kb_entity_get, includes related memories.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntityFull {
    pub id: EntityId,
    pub name: String,
    pub kind: EntityKind,
    pub roles: Vec<String>,
    pub attrs: serde_json::Value,
    pub context: Option<String>,
    pub memory_count: u64,
    pub recent_memories: Vec<MemorySummary>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

