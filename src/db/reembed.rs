//! Re-embedding migration tool.
//!
//! When the embedding model or dimension configuration changes, all existing
//! vectors become stale. This module re-embeds every active memory's summary,
//! processing in batches to avoid overwhelming memory.

use std::sync::Arc;

use tracing::{info, warn};

use crate::db::client::SurrealClient;
use crate::db::embeddings::LocalEmbedder;
use crate::error::{KbError, Result};

/// Result of a re-embedding run.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct ReembedResult {
    /// Number of memories successfully re-embedded.
    pub updated: u64,
    /// Number of memories that failed to re-embed.
    pub failed: u64,
    /// Total memories processed.
    pub total: u64,
}

/// Re-embed all active memories in batches.
///
/// Iterates through all active memories, generates new embeddings from their
/// summaries, and stores the updated vectors. Processes in batches to avoid
/// holding the entire dataset in memory.
pub async fn reembed_all(
    db: &SurrealClient,
    embedder: Arc<LocalEmbedder>,
    batch_size: usize,
) -> Result<ReembedResult> {
    let batch = batch_size.max(1) as i64;
    let mut offset = 0i64;
    let mut updated = 0u64;
    let mut failed = 0u64;

    loop {
        let mut result = db
            .query(
                "SELECT record::id(id) AS mid, summary FROM memory
                 WHERE status = 'active'
                 ORDER BY created_at ASC
                 LIMIT $limit START $offset",
            )
            .bind(("limit", batch))
            .bind(("offset", offset))
            .await
            .map_err(|e| KbError::db("reembed: fetching batch", e))?;

        let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();
        if rows.is_empty() {
            break;
        }

        let batch_count = rows.len() as i64;

        for row in &rows {
            let mid = row
                .get("mid")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let summary = row
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            if mid.is_empty() || summary.is_empty() {
                failed += 1;
                continue;
            }

            match embedder.embed(summary).await {
                Ok(vector) => {
                    let store_result = db
                        .query("UPDATE type::record('memory', $id) SET vector = $vector")
                        .bind(("id", mid.to_string()))
                        .bind(("vector", vector))
                        .await;

                    match store_result {
                        Ok(_) => updated += 1,
                        Err(e) => {
                            warn!(mid, "reembed store failed: {e}");
                            failed += 1;
                        }
                    }
                }
                Err(e) => {
                    warn!(mid, "reembed failed: {e}");
                    failed += 1;
                }
            }
        }

        offset += batch_count;
        info!(processed = offset, updated, failed, "reembed progress");
    }

    let total = updated + failed;
    info!(total, updated, failed, "reembed complete");

    Ok(ReembedResult {
        updated,
        failed,
        total,
    })
}
