//! Generate a compact markdown summary of the knowledge base state.
//!
//! This is the "Layer 1" of the two-layer storage design: a small, always-loaded
//! markdown file (`~/.claude/CLAUDE.md`) that gives every new Claude session
//! instant context without any tool calls.
//!
//! Target: under 400 tokens. Auto-generated; never edited by hand.

use chrono::Utc;
use tracing::instrument;

use crate::config::AlertConfig;
use crate::db::alerts;
use crate::db::client::SurrealClient;
use crate::db::BITEMPORAL_ACTIVE;
use crate::error::{KbError, Result};

/// Generate the compact markdown summary string.
#[instrument(skip_all)]
pub async fn generate_summary(db: &SurrealClient, alert_config: &AlertConfig) -> Result<String> {
    // SurrealDB v3 requires every field referenced in ORDER BY to appear in
    // the SELECT list.  We select the sort columns explicitly even when the
    // rendering logic below does not use them directly.
    let priorities = query_rows(
        db,
        &format!(
            "SELECT summary, importance FROM memory
             WHERE {BITEMPORAL_ACTIVE} AND visibility = 'standard'
               AND memory_type = 'working' AND subtype = 'priority'
             ORDER BY importance DESC LIMIT 3"
        ),
        "markdown: fetch active working/priority memories",
    )
    .await?;

    let key_memories = query_rows(
        db,
        &format!(
            "SELECT summary, memory_type, project, importance, updated_at FROM memory
             WHERE {BITEMPORAL_ACTIVE} AND visibility = 'standard' AND importance >= 4
             ORDER BY importance DESC, updated_at DESC LIMIT 5"
        ),
        "markdown: fetch high-importance memories",
    )
    .await?;

    let action_items = query_rows(
        db,
        &format!(
            "SELECT summary, project, importance, event_at FROM memory
             WHERE {BITEMPORAL_ACTIVE} AND visibility = 'standard'
               AND memory_type = 'prospective' AND subtype = 'action_item'
             ORDER BY importance DESC, event_at ASC LIMIT 5"
        ),
        "markdown: fetch active action items",
    )
    .await?;

    let entities = query_rows(
        db,
        "SELECT name, kind, context, updated_at FROM entity
         ORDER BY updated_at DESC LIMIT 5",
        "markdown: fetch recent entities",
    )
    .await?;

    let alert_list = alerts::check_all(db, alert_config).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "alert fetch failed during markdown regeneration; continuing with empty alerts");
        Vec::new()
    });

    let mut md = String::with_capacity(2048);
    md.push_str("# Knowledge Base Summary\n\n");

    // Priorities
    if !priorities.is_empty() {
        md.push_str("## Current Priorities\n");
        for row in &priorities {
            let summary = row_str(row, "summary");
            md.push_str(&format!("- {summary}\n"));
        }
        md.push('\n');
    }

    // Key memories
    if !key_memories.is_empty() {
        md.push_str("## Key Memories\n");
        for row in &key_memories {
            let summary = row_str(row, "summary");
            let mtype = row_str(row, "memory_type");
            let project = row.get("project").and_then(|v| v.as_str()).unwrap_or("");
            if project.is_empty() {
                md.push_str(&format!("- [{mtype}] {summary}\n"));
            } else {
                md.push_str(&format!("- [{mtype}] {summary} ({project})\n"));
            }
        }
        md.push('\n');
    }

    // Action items
    if !action_items.is_empty() {
        md.push_str("## Open Action Items\n");
        for row in &action_items {
            let summary = row_str(row, "summary");
            md.push_str(&format!("- {summary}\n"));
        }
        md.push('\n');
    }

    // Alerts
    if !alert_list.is_empty() {
        md.push_str("## Needs Attention\n");
        for alert in &alert_list {
            md.push_str(&format!("- {}\n", alert.message));
        }
        md.push('\n');
    }

    // Key entities
    if !entities.is_empty() {
        md.push_str("## Key People & Entities\n");
        for row in &entities {
            let name = row_str(row, "name");
            let kind = row_str(row, "kind");
            let context = row.get("context").and_then(|v| v.as_str()).unwrap_or("");
            if context.is_empty() {
                md.push_str(&format!("- **{name}** ({kind})\n"));
            } else {
                md.push_str(&format!("- **{name}** ({kind}): {context}\n"));
            }
        }
        md.push('\n');
    }

    md.push_str(&format!(
        "*Auto-generated {}*\n",
        Utc::now().format("%Y-%m-%d %H:%M UTC")
    ));

    Ok(md)
}

/// Generate and write the summary to a file path.
///
/// # Path safety
/// `output_path` is always derived from `MarkdownConfig::resolved_path()`, which
/// expands `~/` against `$HOME` and returns a server-controlled `PathBuf`.  The
/// `kb_regenerate_md` MCP tool takes zero user-supplied parameters, so there is
/// no user-controlled path component here.  No `safe_filename` call is needed.
#[instrument(skip(db, alert_config))]
pub async fn regenerate_file(
    db: &SurrealClient,
    alert_config: &AlertConfig,
    output_path: &std::path::Path,
) -> Result<()> {
    let md = generate_summary(db, alert_config).await?;

    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(KbError::Io)?;
    }

    tokio::fs::write(output_path, md.as_bytes())
        .await
        .map_err(KbError::Io)?;

    tracing::info!(path = %output_path.display(), "regenerated CLAUDE.md");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn query_rows(
    db: &SurrealClient,
    sql: &str,
    context: &str,
) -> Result<Vec<serde_json::Value>> {
    let mut result = db
        .query(sql)
        .await
        .map_err(|e| KbError::db(context, e))?;
    let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();
    Ok(rows)
}

fn row_str<'a>(row: &'a serde_json::Value, field: &str) -> &'a str {
    row.get(field).and_then(|v| v.as_str()).unwrap_or("")
}
