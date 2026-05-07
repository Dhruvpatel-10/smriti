use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::config::SpreadingConfig;
use crate::db::embeddings::Embedder;
use crate::db::{client::SurrealClient, recall_metrics, search};
use crate::error::{KbError, Result};
use crate::llm::sidecar::SidecarClient;
use crate::observability;
use crate::types::*;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StatusInput {
    /// Filter by project (if None, returns across all projects)
    pub project: Option<String>,
    /// Filter by domain tag (only memories that include this domain)
    pub domain: Option<String>,
    /// When true, attach a `diagnostics` block: version, uptime, embedder
    /// fingerprint, sidecar circuit state, DB counts, recall stage p95.
    /// Default false — keeps the canonical kb_status response compact.
    #[serde(default)]
    pub include_diagnostics: Option<bool>,
}

/// Read-only references the server passes into `kb_status` so it can build the
/// diagnostics block. Cheap to construct (Arc clones); not stored.
pub struct DiagnosticsCtx<'a> {
    pub embedder: Option<&'a Arc<dyn Embedder>>,
    pub sidecar: Option<&'a Arc<SidecarClient>>,
    pub spreading_config: &'a SpreadingConfig,
}

#[tracing::instrument(
    name = "kb_status",
    skip_all,
    fields(
        project = input.project.as_deref().unwrap_or(""),
        has_domain_filter = input.domain.is_some(),
        include_diagnostics = input.include_diagnostics.unwrap_or(false),
    )
)]
pub async fn execute(
    db: &SurrealClient,
    ctx: DiagnosticsCtx<'_>,
    input: StatusInput,
) -> Result<StatusReport> {
    let mut report =
        search::status_query(db, input.project.as_deref(), input.domain.as_deref()).await?;
    if input.include_diagnostics.unwrap_or(false) {
        report.diagnostics = Some(build_diagnostics(db, &ctx, &report).await?);
    }
    Ok(report)
}

async fn build_diagnostics(
    db: &SurrealClient,
    ctx: &DiagnosticsCtx<'_>,
    report: &StatusReport,
) -> Result<DiagnosticsBlock> {
    let embedder = ctx.embedder.map(|e| {
        let fp = e.fingerprint();
        EmbedderDiagnostics {
            provider: fp.provider,
            model: fp.model,
            dimensions: fp.dimensions as u64,
        }
    });

    let sidecar = ctx.sidecar.map(|s| SidecarDiagnostics {
        circuit_state: match s.circuit_state() {
            crate::llm::CircuitState::Healthy => "healthy".to_string(),
            crate::llm::CircuitState::BackingOff => "backing_off".to_string(),
            crate::llm::CircuitState::Broken => "broken".to_string(),
        },
        restart_count: s.restart_count(),
        dropped_inflight: s.dropped_inflight(),
        last_success_age_secs: s.last_success_age().map(|d| d.as_secs()),
    });

    let migrations_applied = count_migrations(db).await.unwrap_or(0);
    let total_entities = count_table(db, "entity").await.unwrap_or(0);
    let total_agents = count_table(db, "agent").await.unwrap_or(0);

    Ok(DiagnosticsBlock {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: observability::uptime().map(|d| d.as_secs()).unwrap_or(0),
        embedder,
        sidecar,
        db: DbDiagnostics {
            migrations_applied,
            total_memories: report.total_memories,
            active_memories: report.active_memories,
            total_entities,
            total_agents,
        },
        recall_stage_p95: recall_metrics::snapshot_and_reset(),
        spreading_config: ctx.spreading_config.clone(),
    })
}

async fn count_migrations(db: &SurrealClient) -> Result<u64> {
    let mut res = db
        .query("SELECT count() AS n FROM _kb_migrations GROUP ALL")
        .await
        .map_err(|e| KbError::db("counting migrations", e))?;
    let row: Option<serde_json::Value> = res
        .take(0)
        .map_err(|e| KbError::db("deserializing migration count", e))?;
    Ok(row
        .as_ref()
        .and_then(|v| v.get("n"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0))
}

async fn count_table(db: &SurrealClient, table: &str) -> Result<u64> {
    // Table name comes from a static literal in this module — no SQL injection
    // surface — but we still keep the function private to avoid future misuse.
    let sql = format!("SELECT count() AS n FROM {table} GROUP ALL");
    let mut res = db
        .query(&sql)
        .await
        .map_err(|e| KbError::db("counting rows", e))?;
    let row: Option<serde_json::Value> = res
        .take(0)
        .map_err(|e| KbError::db("deserializing row count", e))?;
    Ok(row
        .as_ref()
        .and_then(|v| v.get("n"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0))
}
