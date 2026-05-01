use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};

use crate::config::{AlertConfig, AppConfig, ExportConfig, FusionConfig, MarkdownConfig, SpreadingConfig};
use crate::db::client::SurrealClient;
use crate::db::embeddings::{build_embedder, Embedder};
use crate::db::reranker::Reranker;
use crate::db::reranker::voyage::VoyageReranker;
use crate::shutdown::TaskRegistry;
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
    spreading_config: SpreadingConfig,
    fusion_config: FusionConfig,
    embedding_client: Option<Arc<dyn Embedder>>,
    reranker: Option<Arc<Reranker>>,
    llm_sidecar: Option<Arc<crate::llm::sidecar::SidecarClient>>,
    pub tasks: Arc<TaskRegistry>,
    tool_router: ToolRouter<Self>,
}

impl KbServer {
    pub async fn new(
        db: SurrealClient,
        config: &AppConfig,
        base_dir: &std::path::Path,
        tasks: Arc<TaskRegistry>,
    ) -> crate::error::Result<(Self, Option<crate::llm::sidecar::ServiceHandle>)> {
        let embedding_client: Option<Arc<dyn Embedder>> = if config.embeddings.enabled {
            match config.embeddings.provider {
                crate::config::EmbeddingProvider::Local => {
                    tracing::info!(
                        model = %config.embeddings.model,
                        "local embedding model enabled (loads on first use)"
                    );
                }
                crate::config::EmbeddingProvider::Voyage => {
                    let vcfg = config.embeddings.voyage.as_ref().cloned().unwrap_or_default();
                    tracing::info!(
                        model = %vcfg.model,
                        dimensions = vcfg.output_dimension,
                        "Voyage AI embedder selected"
                    );
                }
            }
            Some(build_embedder(&config.embeddings)?)
        } else {
            tracing::warn!("embeddings disabled — no dedup, no auto-linking, no vector search. Set embeddings.enabled = true in config.toml to enable");
            None
        };

        // Embedder fingerprint gate: refuse to serve when stored vectors and
        // current configuration disagree on (provider, model, dim). On a
        // first-run or fresh DB, records the new fingerprint and proceeds.
        // See src/db/embeddings/fingerprint.rs.
        if let Some(ref e) = embedding_client {
            let fp = e.fingerprint();
            crate::db::embeddings::fingerprint::check_or_initialize(&db, &fp).await?;
        }

        let reranker = if let (Some(embedder), Some(voyage_cfg)) =
            (&embedding_client, config.embeddings.voyage.as_ref())
            && embedder.fingerprint().provider == "voyage"
            && voyage_cfg.reranker.enabled
        {
            let api_key = std::env::var("VOYAGE_API_KEY")
                .ok()
                .or_else(|| voyage_cfg.api_key.clone())
                .ok_or_else(|| {
                    crate::error::KbError::Config(
                        "VOYAGE_API_KEY required for reranker".to_string(),
                    )
                })?;
            Some(Arc::new(Reranker::Voyage(
                Arc::new(VoyageReranker::new(api_key, &voyage_cfg.reranker)?),
                voyage_cfg.reranker.policy,
            )))
        } else {
            None
        };

        let (llm_sidecar, main_sidecar_service) = if config.llm.enabled {
            tracing::info!(
                sidecar_path = %config.llm.sidecar_path.display(),
                confidence_threshold = config.llm.confidence_threshold,
                "LLM sidecar enabled"
            );
            match crate::llm::sidecar::SidecarClient::new(config.llm.clone(), base_dir.to_path_buf(), tasks.cancellation_token()).await {
                Ok((client, service)) => (Some(client), Some(service)),
                Err(e) => {
                    tracing::warn!(error = %e, "LLM sidecar startup failed — proceeding without typed-relation extraction");
                    (None, None)
                }
            }
        } else {
            tracing::info!("LLM sidecar disabled — no typed-relation extraction");
            (None, None)
        };

        Ok((Self {
            db,
            queue_dir: config.queue_dir()?,
            export_dir: config.export_dir(base_dir),
            max_queue_size: config.queue.max_queue_size,
            alert_config: AlertConfig {
                overdue_action_item_days: config.alerts.overdue_action_item_days,
                stale_contact_days: config.alerts.stale_contact_days,
                expiring_memory_hours: config.alerts.expiring_memory_hours,
                extraction_backlog_threshold: config.alerts.extraction_backlog_threshold,
            },
            export_config: ExportConfig {
                backup_dir: config.export.backup_dir.clone(),
                exclude_sensitive: config.export.exclude_sensitive,
            },
            markdown_config: config.markdown.clone(),
            spreading_config: config.search.spreading.clone(),
            fusion_config: config.search.fusion.clone(),
            embedding_client,
            reranker,
            llm_sidecar,
            tasks,
            tool_router: Self::tool_router(),
        }, main_sidecar_service))
    }
}

#[tool_router(router = tool_router)]
impl KbServer {
    // ── Core memory operations ───────────────────────────────────────

    /// Store a new memory with summary, type, and metadata. Returns the memory ID.
    #[tool(name = "kb_remember", description = "Store a new memory with summary, type, and metadata")]
    async fn kb_remember(&self, params: Parameters<remember::RememberInput>) -> String {
        match remember::execute(&self.db, self.embedding_client.clone(), self.llm_sidecar.clone(), self.tasks.clone(), params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Search memories by query, type, project, domain, person, or time range.
    #[tool(name = "kb_recall", description = "Search memories by query, type, project, domain, person, or time range. Returns summaries only.")]
    async fn kb_recall(&self, params: Parameters<recall::RecallInput>) -> String {
        match recall::execute(&self.db, self.embedding_client.clone(), self.reranker.clone(), params.0, &self.spreading_config, &self.fusion_config).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Point-in-time recall: return what was active at a specific moment.
    #[tool(name = "kb_recall_at", description = "Point-in-time recall: return what was active at a specific moment in history")]
    async fn kb_recall_at(&self, params: Parameters<recall_at::RecallAtInput>) -> String {
        match recall_at::execute(&self.db, self.embedding_client.clone(), self.reranker.clone(), params.0, &self.fusion_config).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Fetch the full content of a single memory by ID.
    #[tool(name = "kb_drill", description = "Fetch the full content of a single memory by ID")]
    async fn kb_drill(&self, params: Parameters<drill::DrillInput>) -> String {
        match drill::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Archive a memory (soft delete — never hard-deletes). Use reason=forget for "no longer relevant" or reason=retract for "explicitly wrong".
    #[tool(name = "kb_archive", description = "Archive a memory. reason=forget means 'no longer relevant'; reason=retract means 'I was wrong' and optionally links a correction memory via retracts edge.")]
    async fn kb_archive(&self, params: Parameters<archive::ArchiveInput>) -> String {
        match archive::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Promote a short-term memory to long-term (clears TTL, bumps importance).
    #[tool(name = "kb_promote", description = "Promote a short-term memory to long-term (clears TTL, bumps importance)")]
    async fn kb_promote(&self, params: Parameters<promote::PromoteInput>) -> String {
        match promote::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    // ── Entity operations ────────────────────────────────────────────

    /// Create or update a flexible entity (person, company, concept, etc.).
    #[tool(name = "kb_entity_upsert", description = "Create or update a flexible entity (person, company, product, concept, project)")]
    async fn kb_entity_upsert(&self, params: Parameters<entity::EntityUpsertInput>) -> String {
        match entity::execute_upsert(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Search entities by name, kind, or role.
    #[tool(name = "kb_entity_find", description = "Search entities by name, kind, or role")]
    async fn kb_entity_find(&self, params: Parameters<entity::EntityFindInput>) -> String {
        match entity::execute_find(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Get full entity details including related memories.
    #[tool(name = "kb_entity_get", description = "Get full entity details including related memories")]
    async fn kb_entity_get(&self, params: Parameters<entity::EntityGetInput>) -> String {
        match entity::execute_get(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Soft-retract a typed_relation edge (LLM-extracted). Edge stays in DB but is filtered from spreading activation.
    #[tool(name = "kb_relation_retract", description = "Soft-retract a typed_relation edge (LLM-extracted). Edge stays in DB but is filtered from spreading activation.")]
    async fn kb_relation_retract(&self, params: Parameters<relation_retract::RelationRetractInput>) -> String {
        match relation_retract::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Soft-delete (archive) an entity. Entity stays in DB but is hidden from all read paths.
    #[tool(name = "kb_entity_delete", description = "Soft-delete (archive) an entity. Entity stays in DB but is hidden from all normal read paths. Idempotent.")]
    async fn kb_entity_delete(&self, params: Parameters<entity_delete::EntityDeleteInput>) -> String {
        match entity_delete::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    // ── Agent operations ─────────────────────────────────────────────

    /// Register a new agent (LLM author) or resurrect an archived one.
    #[tool(name = "kb_agent_register", description = "Register a new agent (LLM author) or resurrect an archived one.")]
    async fn kb_agent_register(&self, params: Parameters<agent::AgentRegisterInput>) -> String {
        match agent::execute_register(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Retire (soft-archive) an agent by id or name. Optional purge_memories archives memories too.
    #[tool(name = "kb_agent_retire", description = "Retire (soft-archive) an agent by id or name. Optional purge_memories archives memories too.")]
    async fn kb_agent_retire(&self, params: Parameters<agent::AgentRetireInput>) -> String {
        match agent::execute_retire(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Query agents. Pass id_or_name to fetch a single AgentFull; omit it to list AgentSummary records.
    #[tool(name = "kb_agent_query", description = "Query agents. With id_or_name: returns single AgentFull. Without: returns AgentSummary list, filterable by project and include_archived.")]
    async fn kb_agent_query(&self, params: Parameters<agent::AgentQueryInput>) -> String {
        match agent::execute_query(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    // ── Graph & organization ─────────────────────────────────────────

    /// Create a semantic link between two memories.
    #[tool(name = "kb_link", description = "Create a semantic link between two memories")]
    async fn kb_link(&self, params: Parameters<link::LinkInput>) -> String {
        match link::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Merge multiple memories into one consolidated memory, archiving the originals.
    #[tool(name = "kb_consolidate", description = "Merge multiple memories into one consolidated memory, archiving the originals")]
    async fn kb_consolidate(&self, params: Parameters<consolidate::ConsolidateInput>) -> String {
        match consolidate::execute(&self.db, self.embedding_client.clone(), params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Queue a natural language instruction for the memory session.
    #[tool(name = "kb_queue", description = "Queue a natural language instruction for the memory session to process later")]
    async fn kb_queue(&self, params: Parameters<queue::QueueInput>) -> String {
        match queue::execute(&self.queue_dir, self.max_queue_size, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    // ── Status & maintenance ─────────────────────────────────────────

    /// Get status report: open action items, open questions, recent decisions, alerts.
    #[tool(name = "kb_status", description = "Get status report: open action items, open questions, recent decisions, alerts")]
    async fn kb_status(&self, params: Parameters<status::StatusInput>) -> String {
        match status::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Profile snapshot: stable static facts + recent dynamic activity.
    #[tool(name = "kb_profile", description = "Profile snapshot for fast LLM-context injection: stable static facts (preferences, values, life goals) plus recent dynamic activity")]
    async fn kb_profile(&self, params: Parameters<profile::ProfileInput>) -> String {
        match profile::execute(&self.db, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Check for overdue items, stale contacts, and expiring memories.
    #[tool(name = "kb_alerts", description = "Check for overdue items, stale contacts, expiring memories, and potential contradictions")]
    async fn kb_alerts(&self) -> String {
        match alerts::execute(&self.db, &self.alert_config).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
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
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Import memories and entities from a JSONL backup file.
    #[tool(name = "kb_import", description = "Import memories and entities from a JSONL backup file")]
    async fn kb_import(&self, params: Parameters<export::ImportInput>) -> String {
        match export::execute_import(&self.db, &self.export_dir, params.0).await {
            Ok(out) => serde_json::to_string(&out).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    // ── New maintenance tools ────────────────────────────────────────

    /// Regenerate the CLAUDE.md markdown summary from the current knowledge base state.
    #[tool(name = "kb_regenerate_md", description = "Regenerate the CLAUDE.md markdown summary file from current KB state")]
    #[tracing::instrument(name = "kb_regenerate_md", skip_all)]
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
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Find clusters of related memories that are candidates for consolidation.
    #[tool(name = "kb_consolidation_candidates", description = "Find clusters of related memories that should be consolidated (merged). Returns suggestions — does not auto-merge.")]
    #[tracing::instrument(
        name = "kb_consolidation_candidates",
        skip_all,
        fields(limit = params.0.limit)
    )]
    async fn kb_consolidation_candidates(
        &self,
        params: Parameters<ConsolidationCandidatesInput>,
    ) -> String {
        // u8 → usize is infallible: u8 max (255) always fits in usize.
        let limit = usize::from(params.0.limit.unwrap_or(10).min(50));
        match crate::db::consolidation::find_candidates(&self.db, limit, None, None).await {
            Ok(candidates) => serde_json::json!({
                "candidates": candidates,
                "count": candidates.len()
            })
            .to_string(),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }

    /// Re-embed all active memories with the current embedding model. Use after changing model or dimensions.
    #[tool(name = "kb_reembed", description = "Re-embed all active memories with the current embedding model. Use after changing model or dimensions in config.")]
    #[tracing::instrument(name = "kb_reembed", skip_all)]
    async fn kb_reembed(&self) -> String {
        let Some(ref embedder) = self.embedding_client else {
            return r#"{"error": "embeddings are disabled in config"}"#.to_string();
        };
        match crate::db::reembed::reembed_all(&self.db, embedder.clone(), 50).await {
            Ok(result) => serde_json::to_string(&result).unwrap_or_else(|e| serde_json::json!({"error": format!("serialization failed: {}", e)}).to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "tool call failed");
                serde_json::json!({"error": e.client_message()}).to_string()
            }
        }
    }
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
