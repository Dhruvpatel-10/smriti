//! Automated consolidation candidate detection.
//!
//! Identifies clusters of related memories that should be merged, but does NOT
//! auto-merge — actual consolidation requires LLM reasoning to produce a good
//! summary. This module surfaces candidates for the memory session to act on.

use serde::Serialize;
use tracing::instrument;

use crate::db::client::SurrealClient;
use crate::error::{KbError, Result};

/// A group of memories that are candidates for consolidation.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ConsolidationCandidate {
    /// Memory IDs in this cluster.
    pub ids: Vec<String>,
    /// Summaries of the memories (for LLM context).
    pub summaries: Vec<String>,
    /// Why these were grouped together.
    pub reason: String,
    /// Shared project (if any).
    pub project: Option<String>,
}

/// Find clusters of memories that are candidates for consolidation.
///
/// Heuristics:
/// 1. Same project + same subtype with 3+ active memories → likely related
/// 2. Connected graph clusters (3+ memories linked via `relates` edges)
#[instrument(skip(db))]
pub async fn find_candidates(
    db: &SurrealClient,
    limit: usize,
) -> Result<Vec<ConsolidationCandidate>> {
    let mut candidates = Vec::new();

    // Heuristic 1: Same-project, same-subtype clusters with 3+ members.
    // SurrealDB v3 does not support HAVING — filter count >= 3 in Rust.
    let mut result = db
        .query(
            "SELECT project, subtype,
                    array::group(record::id(id)) AS ids,
                    array::group(summary) AS summaries,
                    count() AS cnt
             FROM memory
             WHERE status = 'active'
               AND visibility != 'sensitive'
               AND project != NONE
             GROUP BY project, subtype",
        )
        .await
        .map_err(|e| KbError::db("consolidation: project+subtype clusters", e))?;

    let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();

    for row in rows {
        let cnt = row.get("cnt").and_then(|v| v.as_u64()).unwrap_or(0);
        if cnt < 3 {
            continue;
        }
        let project = row.get("project").and_then(|v| v.as_str()).map(String::from);
        let subtype = row
            .get("subtype")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let ids = extract_string_array(&row, "ids");
        let summaries = extract_string_array(&row, "summaries");

        candidates.push(ConsolidationCandidate {
            ids,
            summaries,
            reason: format!(
                "Same project + subtype ({subtype}) with 3+ memories"
            ),
            project,
        });
    }

    // Heuristic 2: Graph clusters — memories with 2+ `relates` edges.
    // Standard-only: consolidation candidates are always cross-project.
    let mut result = db
        .query(
            "SELECT record::id(id) AS mid, summary,
                    array::group(record::id(->relates->memory.id)) AS related
             FROM memory
             WHERE status = 'active'
               AND visibility = 'standard'
             LIMIT 200",
        )
        .await
        .map_err(|e| KbError::db("consolidation: graph clusters", e))?;

    let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();

    // Build id→summary lookup in one pass to avoid O(n²) nested scan.
    let summary_map: std::collections::HashMap<&str, &str> = rows
        .iter()
        .filter_map(|r| {
            let mid = r.get("mid").and_then(|v| v.as_str())?;
            let summary = r.get("summary").and_then(|v| v.as_str()).unwrap_or_default();
            Some((mid, summary))
        })
        .collect();

    for row in &rows {
        let mid = row.get("mid").and_then(|v| v.as_str()).unwrap_or_default();
        let summary = row
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let related = extract_string_array(row, "related");

        if related.len() >= 2 && !mid.is_empty() {
            let mut ids = vec![mid.to_string()];
            let mut summaries = vec![summary.to_string()];

            for rid in &related {
                ids.push(rid.clone());
                if let Some(&s) = summary_map.get(rid.as_str()) {
                    summaries.push(s.to_string());
                }
            }

            candidates.push(ConsolidationCandidate {
                ids,
                summaries,
                reason: "Graph cluster with 3+ related memories".to_string(),
                project: None,
            });
        }
    }

    candidates.truncate(limit);
    Ok(candidates)
}

fn extract_string_array(row: &serde_json::Value, field: &str) -> Vec<String> {
    row.get(field)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}
