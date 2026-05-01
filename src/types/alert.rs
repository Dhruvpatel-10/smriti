use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

use super::ids::{EntityId, MemoryId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlertType {
    OverdueActionItem,
    StaleContact,
    ExpiringMemory,
    Contradiction,
    /// More than `alerts.extraction_backlog_threshold` memories have a failed
    /// extraction status (anchor_failed, relation_failed, both_failed, or
    /// sidecar_unavailable). Run `--retry-failed-extraction` to clear.
    ExtractionBacklog,
}

impl fmt::Display for AlertType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OverdueActionItem => write!(f, "overdue_action_item"),
            Self::StaleContact => write!(f, "stale_contact"),
            Self::ExpiringMemory => write!(f, "expiring_memory"),
            Self::Contradiction => write!(f, "contradiction"),
            Self::ExtractionBacklog => write!(f, "extraction_backlog"),
        }
    }
}

/// Typed reference carried by an alert. Prevents callers from confusing a
/// `MemoryId` with an entity name or an `EntityId`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum AlertReference {
    /// Alert relates to a specific memory record.
    Memory(MemoryId),
    /// Alert relates to a specific entity record.
    Entity(EntityId),
    /// Alert relates to an entity identified only by name (e.g. stale-contact
    /// checks, which query entity names but not record IDs).
    EntityName(String),
    /// Alert is aggregate (e.g. ExtractionBacklog) with a numeric count rather
    /// than a specific record reference. Stored as a string for uniformity.
    Count(String),
}

impl AlertReference {
    /// Raw string representation used for equality checks in tests and logging.
    /// Returns the inner string of whichever variant is active.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Memory(id) => id.as_str(),
            Self::Entity(id) => id.as_str(),
            Self::EntityName(name) => name.as_str(),
            Self::Count(s) => s.as_str(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Alert {
    pub alert_type: AlertType,
    pub message: String,
    /// Typed reference to the memory or entity this alert relates to.
    pub reference: AlertReference,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_type_display() {
        assert_eq!(format!("{}", AlertType::OverdueActionItem), "overdue_action_item");
        assert_eq!(format!("{}", AlertType::StaleContact), "stale_contact");
    }

    #[test]
    fn alert_type_serde_roundtrip() {
        let all = [
            AlertType::OverdueActionItem,
            AlertType::StaleContact,
            AlertType::ExpiringMemory,
            AlertType::Contradiction,
            AlertType::ExtractionBacklog,
        ];
        for v in &all {
            let json = serde_json::to_string(v).unwrap();
            let back: AlertType = serde_json::from_str(&json).unwrap();
            assert_eq!(*v, back);
        }
    }
}
