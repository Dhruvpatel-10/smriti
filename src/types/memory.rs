use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::enums::*;
use super::ids::MemoryId;

/// Full memory record as stored in DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: MemoryId,
    pub summary: String,
    pub content: Option<String>,
    pub memory_type: MemoryType,
    pub subtype: MemorySubtype,
    pub source: Source,
    pub source_ref: Option<String>,
    pub project: Option<String>,
    pub domain: Vec<String>,
    pub people: Vec<String>,
    pub importance: Importance,
    pub visibility: Visibility,
    pub status: MemoryStatus,
    pub tags: Vec<String>,
    pub access_count: u32,
    pub event_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub source_agent: Option<String>,
    pub session_id: Option<String>,
}

/// Summary returned by kb_recall -- lightweight.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemorySummary {
    pub id: MemoryId,
    pub summary: String,
    pub memory_type: MemoryType,
    pub subtype: MemorySubtype,
    pub project: Option<String>,
    pub domain: Vec<String>,
    pub importance: Importance,
    pub event_at: DateTime<Utc>,
    pub tags: Vec<String>,
}

/// Full record returned by kb_drill.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemoryFull {
    pub id: MemoryId,
    pub summary: String,
    pub content: Option<String>,
    pub memory_type: MemoryType,
    pub subtype: MemorySubtype,
    pub source: Source,
    pub source_ref: Option<String>,
    pub project: Option<String>,
    pub domain: Vec<String>,
    pub people: Vec<String>,
    pub importance: Importance,
    pub visibility: Visibility,
    pub status: MemoryStatus,
    pub tags: Vec<String>,
    pub access_count: u32,
    pub event_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub source_agent: Option<String>,
    pub session_id: Option<String>,
}

impl From<Memory> for MemorySummary {
    fn from(m: Memory) -> Self {
        Self {
            id: m.id,
            summary: m.summary,
            memory_type: m.memory_type,
            subtype: m.subtype,
            project: m.project,
            domain: m.domain,
            importance: m.importance,
            event_at: m.event_at,
            tags: m.tags,
        }
    }
}

impl From<Memory> for MemoryFull {
    fn from(m: Memory) -> Self {
        Self {
            id: m.id,
            summary: m.summary,
            content: m.content,
            memory_type: m.memory_type,
            subtype: m.subtype,
            source: m.source,
            source_ref: m.source_ref,
            project: m.project,
            domain: m.domain,
            people: m.people,
            importance: m.importance,
            visibility: m.visibility,
            status: m.status,
            tags: m.tags,
            access_count: m.access_count,
            event_at: m.event_at,
            created_at: m.created_at,
            updated_at: m.updated_at,
            expires_at: m.expires_at,
            source_agent: m.source_agent,
            session_id: m.session_id,
        }
    }
}
