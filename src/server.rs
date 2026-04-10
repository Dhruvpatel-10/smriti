use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};

use crate::config::{AlertConfig, AppConfig, ExportConfig, MarkdownConfig};
use crate::db::client::SurrealClient;
use crate::db::embeddings::LocalEmbedder;
use crate::tools::*;

#[derive(Clone)]
pub struct KbServer {
    db: SurrealClient,
    queue_dir: PathBuf,
    export_dir: PathBuf,
    max_queue_size: usize,
    alert_config: AlertConfig,
    export_config: ExportConfig,
    markdown_config: MarkdownConfig,
    embedding_client: Option<Arc<LocalEmbedder>>,
    tool_router: ToolRouter<Self>,
}

impl KbServer {
    pub fn new(db: SurrealClient, config: &AppConfig, base_dir: &std::path::Path) -> Self {
        let embedding_client = if config.embeddings.enabled {
            tracing::info!(
                model = %config.embeddings.model,
                "local embedding model enabled (loads on first use)"
            );
            Some(Arc::new(LocalEmbedder::new(&config.embeddings)))
        } else {
            tracing::warn!("embeddings disabled — no dedup, no auto-linking, no vector search. Set embeddings.enabled = true in config.toml to enable");
            None
        };

        Self {
            db,
            queue_dir: config.queue_dir(),
            export_dir: config.export_dir(base_dir),
            max_queue_size: config.queue.max_queue_size,
            alert_config: AlertConfig {
                overdue_action_item_days: config.alerts.overdue_action_item_days,
                stale_contact_days: config.alerts.stale_contact_days,
                expiring_memory_hours: config.alerts.expiring_memory_hours,
            },
            export_config: ExportConfig {
                backup_dir: config.export.backup_dir.clone(),
                exclude_sensitive: config.export.exclude_sensitive,
            },
            markdown_config: config.markdown.clone(),
            embedding_client,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router(router = tool_router)]
impl KbServer {
    // ── Core memory operations ───────────────────────────────────────

    /// Store a new memory with summary, type, and metadata. Returns the memory ID.
    #[tool(name = "kb_remember", description = "Store a new memory with summary, type, and metadata")]
    async fn kb_remember(&self, params: Parameters<remember::RememberInput>) -> String {
        match remember::execute(&self.db, self.embedding_client.clone(), params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Search memories by query, type, project, domain, person, or time range.
    #[tool(name = "kb_recall", description = "Search memories by query, type, project, domain, person, or time range. Returns summaries only.")]
    async fn kb_recall(&self, params: Parameters<recall::RecallInput>) -> String {
        match recall::execute(&self.db, self.embedding_client.clone(), params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Point-in-time recall: return what was active at a specific moment.
    #[tool(name = "kb_recall_at", description = "Point-in-time recall: return what was active at a specific moment in history")]
    async fn kb_recall_at(&self, params: Parameters<recall_at::RecallAtInput>) -> String {
        match recall_at::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Fetch the full content of a single memory by ID.
    #[tool(name = "kb_drill", description = "Fetch the full content of a single memory by ID")]
    async fn kb_drill(&self, params: Parameters<drill::DrillInput>) -> String {
        match drill::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Archive a memory (soft delete — never hard-deletes).
    #[tool(name = "kb_forget", description = "Archive a memory (soft delete, never hard-deletes)")]
    async fn kb_forget(&self, params: Parameters<forget::ForgetInput>) -> String {
        match forget::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Promote a short-term memory to long-term (clears TTL, bumps importance).
    #[tool(name = "kb_promote", description = "Promote a short-term memory to long-term (clears TTL, bumps importance)")]
    async fn kb_promote(&self, params: Parameters<promote::PromoteInput>) -> String {
        match promote::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    // ── Entity operations ────────────────────────────────────────────

    /// Create or update a flexible entity (person, company, concept, etc.).
    #[tool(name = "kb_entity_upsert", description = "Create or update a flexible entity (person, company, product, concept, project)")]
    async fn kb_entity_upsert(&self, params: Parameters<entity::EntityUpsertInput>) -> String {
        match entity::execute_upsert(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Search entities by name, kind, or role.
    #[tool(name = "kb_entity_find", description = "Search entities by name, kind, or role")]
    async fn kb_entity_find(&self, params: Parameters<entity::EntityFindInput>) -> String {
        match entity::execute_find(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Get full entity details including related memories.
    #[tool(name = "kb_entity_get", description = "Get full entity details including related memories")]
    async fn kb_entity_get(&self, params: Parameters<entity::EntityGetInput>) -> String {
        match entity::execute_get(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    // ── Graph & organization ─────────────────────────────────────────

    /// Create a semantic link between two memories.
    #[tool(name = "kb_link", description = "Create a semantic link between two memories")]
    async fn kb_link(&self, params: Parameters<link::LinkInput>) -> String {
        match link::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Merge multiple memories into one consolidated memory, archiving the originals.
    #[tool(name = "kb_consolidate", description = "Merge multiple memories into one consolidated memory, archiving the originals")]
    async fn kb_consolidate(&self, params: Parameters<consolidate::ConsolidateInput>) -> String {
        match consolidate::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Queue a natural language instruction for the memory session.
    #[tool(name = "kb_queue", description = "Queue a natural language instruction for the memory session to process later")]
    async fn kb_queue(&self, params: Parameters<queue::QueueInput>) -> String {
        match queue::execute(&self.queue_dir, self.max_queue_size, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    // ── Status & maintenance ─────────────────────────────────────────

    /// Get status report: open action items, open questions, recent decisions, alerts.
    #[tool(name = "kb_status", description = "Get status report: open action items, open questions, recent decisions, alerts")]
    async fn kb_status(&self, params: Parameters<status::StatusInput>) -> String {
        match status::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Check for overdue items, stale contacts, and expiring memories.
    #[tool(name = "kb_alerts", description = "Check for overdue items, stale contacts, expiring memories, and potential contradictions")]
    async fn kb_alerts(&self) -> String {
        match alerts::execute(&self.db, &self.alert_config).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Export all memories and entities to a JSONL backup file.
    #[tool(name = "kb_export", description = "Export all memories and entities to a JSONL backup file")]
    async fn kb_export(&self, params: Parameters<export::ExportInput>) -> String {
        match export::execute_export(
            &self.db,
            &self.export_dir,
            self.export_config.exclude_sensitive,
            params.0,
        )
        .await
        {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Import memories and entities from a JSONL backup file.
    #[tool(name = "kb_import", description = "Import memories and entities from a JSONL backup file")]
    async fn kb_import(&self, params: Parameters<export::ImportInput>) -> String {
        match export::execute_import(&self.db, &self.export_dir, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Look up person entities by name.
    #[tool(name = "kb_people", description = "Look up person entities by name (searches entity table, kind=person)")]
    async fn kb_people(&self, params: Parameters<PeopleInput>) -> String {
        match crate::db::entity::find(&self.db, &params.0.name, Some("person"), None).await {
            Ok(entities) => serde_json::to_string(&entities).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    // ── New maintenance tools ────────────────────────────────────────

    /// Regenerate the CLAUDE.md markdown summary from the current knowledge base state.
    #[tool(name = "kb_regenerate_md", description = "Regenerate the CLAUDE.md markdown summary file from current KB state")]
    async fn kb_regenerate_md(&self) -> String {
        let path = self.markdown_config.resolved_path();
        match crate::db::markdown::regenerate_file(&self.db, &self.alert_config, &path).await {
            Ok(()) => serde_json::json!({
                "message": "CLAUDE.md regenerated",
                "path": path.display().to_string()
            })
            .to_string(),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Find clusters of related memories that are candidates for consolidation.
    #[tool(name = "kb_consolidation_candidates", description = "Find clusters of related memories that should be consolidated (merged). Returns suggestions — does not auto-merge.")]
    async fn kb_consolidation_candidates(
        &self,
        params: Parameters<ConsolidationCandidatesInput>,
    ) -> String {
        let limit = params.0.limit.unwrap_or(10).min(50) as usize;
        match crate::db::consolidation::find_candidates(&self.db, limit).await {
            Ok(candidates) => serde_json::json!({
                "candidates": candidates,
                "count": candidates.len()
            })
            .to_string(),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }

    /// Re-embed all active memories with the current embedding model. Use after changing model or dimensions.
    #[tool(name = "kb_reembed", description = "Re-embed all active memories with the current embedding model. Use after changing model or dimensions in config.")]
    async fn kb_reembed(&self) -> String {
        let Some(ref embedder) = self.embedding_client else {
            return r#"{"error": "embeddings are disabled in config"}"#.to_string();
        };
        match crate::db::reembed::reembed_all(&self.db, embedder.clone(), 50).await {
            Ok(result) => serde_json::to_string(&result).unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}")),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                format!("{{\"error\": \"{}\"}}", e.client_message())
            }
        }
    }
}

/// Input for kb_people tool (legacy).
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PeopleInput {
    /// Name to search for
    pub name: String,
}

/// Input for kb_consolidation_candidates tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ConsolidationCandidatesInput {
    /// Max number of candidate clusters to return (default 10, max 50)
    pub limit: Option<u8>,
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for KbServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Knowledge Base — personal second brain MCP server. Store, search, and manage memories with full-text search, vector similarity, graph relations, flexible entities, domain tagging, and temporal queries.")
    }
}
