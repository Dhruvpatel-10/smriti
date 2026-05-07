use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::alert::Alert;
use super::memory::MemorySummary;
use crate::config::SpreadingConfig;
use crate::db::recall_metrics::RecallStagePercentiles;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StatusReport {
    pub open_action_items: Vec<MemorySummary>,
    pub open_questions: Vec<MemorySummary>,
    pub recent_decisions: Vec<MemorySummary>,
    pub alerts: Vec<Alert>,
    pub total_memories: u64,
    pub active_memories: u64,
    /// Operator/Claude-Code-debug detail block, populated only when the caller
    /// passes `include_diagnostics=true`. Omitted otherwise to keep the default
    /// `kb_status` response cheap and content-focused.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub diagnostics: Option<DiagnosticsBlock>,
}

/// Single structured diagnostics payload (ADR-028, Step 5). Surfaced via
/// `kb_status(include_diagnostics=true)` instead of a separate MCP tool —
/// keeps the tool surface small and lets a single call answer "is the daemon
/// healthy and up to date?".
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DiagnosticsBlock {
    pub version: String,
    pub uptime_secs: u64,
    pub embedder: Option<EmbedderDiagnostics>,
    pub sidecar: Option<SidecarDiagnostics>,
    pub db: DbDiagnostics,
    pub recall_stage_p95: Option<RecallStagePercentiles>,
    /// Active spreading activation configuration (ADR-029, Change 5).
    /// Allows operators to verify runtime values without inspecting config files.
    pub spreading_config: SpreadingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EmbedderDiagnostics {
    pub provider: String,
    pub model: String,
    pub dimensions: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SidecarDiagnostics {
    /// "healthy" | "backing_off" | "broken" — see `llm::CircuitState`.
    pub circuit_state: String,
    pub restart_count: u64,
    pub dropped_inflight: u64,
    /// Seconds since last successful sidecar RPC reply. `null` when sidecar
    /// has never replied (cold start or process never recovered).
    pub last_success_age_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DbDiagnostics {
    pub migrations_applied: u64,
    pub total_memories: u64,
    pub active_memories: u64,
    pub total_entities: u64,
    pub total_agents: u64,
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
