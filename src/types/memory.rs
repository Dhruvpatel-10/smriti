use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::enums::*;
use super::ids::{AgentId, MemoryId};

/// Full memory record as stored in DB. Returned by `kb_drill`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Memory {
    pub id: MemoryId,
    pub summary: String,
    pub content: Option<String>,
    pub memory_type: MemoryType,
    pub subtype: MemorySubtype,
    pub channel: Option<Channel>,
    pub modality: Option<Modality>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidated_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidation_reason: Option<InvalidationReason>,
    /// Permanent fact flag — true for value/life_goal/preference and high-importance
    /// fact subtypes. Static memories skip TTL, are surfaced first in `kb_profile`,
    /// and are excluded from importance-decay pruning. Defaults to false.
    #[serde(default)]
    pub is_static: bool,
    /// Top-3 anchor entities extracted from the summary at write time by the LLM sidecar.
    /// Proper nouns, project names, technology names, event names. NULL until populated.
    /// Internal index field — not surfaced in MemorySummary or MCP recall responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_entities: Option<Vec<String>>,
    /// State of the post-create LLM sidecar extraction pipeline (anchors + typed relations).
    /// Set to `pending` at create time; updated by the async background task.
    /// None for legacy rows predating migration 019.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction_status: Option<ExtractionStatus>,
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
