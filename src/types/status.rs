use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::alert::Alert;
use super::memory::MemorySummary;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StatusReport {
    pub open_action_items: Vec<MemorySummary>,
    pub open_questions: Vec<MemorySummary>,
    pub recent_decisions: Vec<MemorySummary>,
    pub alerts: Vec<Alert>,
    pub total_memories: u64,
    pub active_memories: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueEntry {
    pub instruction: String,
    pub source_project: String,
    pub queued_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn queue_entry_roundtrip() {
        let entry = QueueEntry {
            instruction: "remember we chose SurrealDB".to_string(),
            source_project: "knowledge-base".to_string(),
            queued_at: Utc::now(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: QueueEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry.instruction, back.instruction);
        assert_eq!(entry.source_project, back.source_project);
    }
}
