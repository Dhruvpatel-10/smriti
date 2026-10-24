//! Re-embedding migration tool.
//!
//! When the embedding model or dimension configuration changes, all existing
//! vectors become stale. This module re-embeds every active memory's content or
//! summary, processing in batches to avoid overwhelming memory.
//!
//! Chunk identity is detected via the graph structure (`extends` edges), not
//! by inspecting the summary string. This prevents false positives from user
//! memories whose summary genuinely begins "[chunk …]".

use std::collections::HashMap;
use std::sync::Arc;

use tracing::{info, warn};

use crate::db::chunking::prepend_structural_prefix;
use crate::db::client::SurrealClient;
use crate::db::embeddings::Embedder;
use crate::db::BITEMPORAL_ACTIVE;
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
/// Chunk rows are detected via the `extends` inbound-edge count (graph-based,
/// never string-pattern). Chunks with `content` are re-embedded from that
/// content. When the embedder supports batched contextualized embedding
/// (`max_chunks_per_doc > 1`), sibling chunks (same parent) are grouped and
/// embedded in one batched call to restore voyage-context-3 contextualization.
///
/// Non-chunk rows are embedded from the same contextual text the write path uses.
pub async fn reembed_all(
    db: &SurrealClient,
    embedder: Arc<dyn Embedder>,
    batch_size: usize,
) -> Result<ReembedResult> {
    let batch = batch_size.max(1) as i64;
    let mut updated = 0u64;
    let mut failed = 0u64;
    let mut offset = 0i64;

    let supports_batched = embedder.max_chunks_per_doc() > 1;

    // The HNSW index is pinned to one dimension; drop it while vectors are
    // rewritten (possibly in a new dimension) and rebuild it at the end.
    crate::db::vector_index::remove(db).await;

    loop {
        // SELECT fields:
        //   is_chunk: true when this memory has ≥1 OUTBOUND `extends` edge
        //     (it extends some parent). Direction: `RELATE chunk -> extends ->
        //     parent` (see chunking.rs::chunk_and_embed), so chunks are the source
        //     of extends edges, not the target. Graph-based — immune to false
        //     positives from user summaries that happen to start "[chunk …]".
        //   parent_id: first parent RecordId (NONE for non-chunks). Used only when
        //     grouping siblings for batched contextualized re-embed.
        //
        // SurrealDB v3: `->extends->memory` traverses outbound `extends` edges to
        // target memory nodes. `array::len(…)` counts them. Boolean expression in
        // SELECT uses IF…THEN…ELSE to ensure a literal bool field (not a comparison
        // operator result, which some SurrealDB versions serialize unexpectedly).
        let mut result = db
            .query(
                "SELECT
                    record::id(id) AS mid,
                    summary,
                    content,
                    memory_type,
                    subtype,
                    project,
                    domain,
                    people,
                    channel,
                    modality,
                    created_at,
                    IF array::len(->extends->memory) > 0 THEN true ELSE false END AS is_chunk,
                    array::first(->extends->memory.id) AS parent_id
                 FROM memory
                 WHERE BITEMPORAL_ACTIVE_PLACEHOLDER
                 ORDER BY created_at ASC
                 LIMIT $limit START $offset"
                    .replace("BITEMPORAL_ACTIVE_PLACEHOLDER", BITEMPORAL_ACTIVE),
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

        if supports_batched {
            // ── Sibling-batch path (Voyage contextualized endpoint) ──────────
            // Group chunk rows by parent_id; non-chunk rows processed individually.
            let mut chunk_groups: HashMap<String, Vec<&serde_json::Value>> = HashMap::new();
            let mut non_chunk_rows: Vec<&serde_json::Value> = Vec::new();

            for row in &rows {
                let is_chunk = row
                    .get("is_chunk")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if is_chunk {
                    // parent_id is a RecordId JSON value; use its JSON string as a
                    // stable grouping key (siblings share the same parent_id value).
                    let parent_key = match row.get("parent_id") {
                        Some(v) => serde_json::to_string(v).unwrap_or_default(),
                        None => String::new(),
                    };
                    chunk_groups.entry(parent_key).or_default().push(row);
                } else {
                    non_chunk_rows.push(row);
                }
            }

            // Re-embed each sibling group in one contextualized call.
            for (parent_mid, chunk_rows) in &chunk_groups {
                let mids: Vec<&str> = chunk_rows
                    .iter()
                    .filter_map(|r| r.get("mid").and_then(|v| v.as_str()))
                    .collect();

                // Collect content strings; skip rows with missing content.
                let mut valid: Vec<(&serde_json::Value, &str)> = Vec::new();
                for row in chunk_rows {
                    let mid = row.get("mid").and_then(|v| v.as_str()).unwrap_or("");
                    match row.get("content").and_then(|v| v.as_str()) {
                        Some(c) if !c.is_empty() => valid.push((row, c)),
                        _ => {
                            warn!(mid, parent = %parent_mid, "chunk row missing content — skipping reembed");
                            failed += 1;
                        }
                    }
                }
                if valid.is_empty() {
                    continue;
                }

                let contents: Vec<String> = valid.iter().map(|(_, c)| c.to_string()).collect();

                match embedder.embed_document_chunks(&contents).await {
                    Ok(vectors) => {
                        for ((row, _), vector) in valid.iter().zip(vectors) {
                            let mid = row.get("mid").and_then(|v| v.as_str()).unwrap_or("");
                            if mid.is_empty() {
                                failed += 1;
                                continue;
                            }
                            match store_vector(db, mid, vector).await {
                                Ok(()) => updated += 1,
                                Err(e) => {
                                    warn!(mid, "reembed chunk store failed: {e}");
                                    failed += 1;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            parent = %parent_mid,
                            chunk_count = mids.len(),
                            "reembed sibling batch failed: {e}"
                        );
                        failed += valid.len() as u64;
                    }
                }
            }

            // Re-embed non-chunk rows from summary.
            for row in non_chunk_rows {
                let (upd, fail) = reembed_single_from_summary(db, &*embedder, row).await;
                updated += upd;
                failed += fail;
            }
        } else {
            // ── Per-row path (local fastembed, max_chunks_per_doc == 1) ──────
            for row in &rows {
                let is_chunk = row
                    .get("is_chunk")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if is_chunk {
                    let mid = row.get("mid").and_then(|v| v.as_str()).unwrap_or("");
                    let summary = row.get("summary").and_then(|v| v.as_str()).unwrap_or("");

                    match row.get("content").and_then(|v| v.as_str()) {
                        Some(c) if !c.is_empty() => {
                            // Prepend structural prefix matching the chunking pipeline
                            // behaviour for single-chunk backends (ADR-020 §3).
                            let prefixed = prepend_structural_prefix(
                                c,
                                summary,
                                embedder.max_input_tokens(),
                            );
                            match embedder.embed_document(&prefixed).await {
                                Ok(vector) => match store_vector(db, mid, vector).await {
                                    Ok(()) => updated += 1,
                                    Err(e) => {
                                        warn!(mid, "reembed chunk store failed: {e}");
                                        failed += 1;
                                    }
                                },
                                Err(e) => {
                                    warn!(mid, "reembed chunk embed failed: {e}");
                                    failed += 1;
                                }
                            }
                        }
                        _ => {
                            warn!(mid, "chunk row missing content — skipping reembed");
                            failed += 1;
                        }
                    }
                } else {
                    let (upd, fail) = reembed_single_from_summary(db, &*embedder, row).await;
                    updated += upd;
                    failed += fail;
                }
            }
        }

        offset += batch_count;
        info!(processed = offset, updated, failed, "reembed progress");
    }

    let total = updated + failed;
    info!(total, updated, failed, "reembed complete");
    crate::db::vector_index::ensure(db, embedder.dimensions()).await;

    Ok(ReembedResult {
        updated,
        failed,
        total,
    })
}

/// Rebuild the write-path embed text (`contextual_text` + content prefix) from a
/// stored row, so re-embedded vectors match freshly written ones. Falls back to
/// the bare summary when type/subtype cannot be parsed (pre-taxonomy rows).
fn embed_text_from_row(row: &serde_json::Value, summary: &str) -> String {
    fn field<T: serde::de::DeserializeOwned>(row: &serde_json::Value, key: &str) -> Option<T> {
        row.get(key).and_then(|v| serde_json::from_value(v.clone()).ok())
    }
    let (Some(memory_type), Some(subtype)) = (
        field::<crate::types::MemoryType>(row, "memory_type"),
        field::<crate::types::MemorySubtype>(row, "subtype"),
    ) else {
        return summary.to_string();
    };
    let project: Option<String> = field(row, "project");
    let domains: Vec<String> = field(row, "domain").unwrap_or_default();
    let people: Vec<String> = field(row, "people").unwrap_or_default();
    let contextual = crate::db::validation::contextual_text(
        summary,
        memory_type,
        subtype,
        project.as_deref(),
        &domains,
        &people,
        field(row, "channel"),
        field(row, "modality"),
    );
    crate::db::validation::with_content_prefix(
        contextual,
        row.get("content").and_then(|v| v.as_str()),
    )
}

/// Re-embed a single non-chunk row with the same text the write path embeds.
///
/// Returns `(updated, failed)` counts for this one row.
async fn reembed_single_from_summary(
    db: &SurrealClient,
    embedder: &dyn Embedder,
    row: &serde_json::Value,
) -> (u64, u64) {
    let mid = row
        .get("mid")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let summary = row
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    if mid.is_empty() || summary.is_empty() {
        return (0, 1);
    }

    let text = embed_text_from_row(row, summary);
    match embedder.embed_document(&text).await {
        Ok(vector) => match store_vector(db, mid, vector).await {
            Ok(()) => (1, 0),
            Err(e) => {
                warn!(mid, "reembed store failed: {e}");
                (0, 1)
            }
        },
        Err(e) => {
            warn!(mid, "reembed embed failed: {e}");
            (0, 1)
        }
    }
}

/// Write a vector to an existing memory record by string ID.
async fn store_vector(db: &SurrealClient, mid: &str, vector: Vec<f32>) -> Result<()> {
    db.query("UPDATE type::record('memory', $id) SET vector = $vector")
        .bind(("id", mid.to_string()))
        .bind(("vector", vector))
        .await
        .map_err(|e| KbError::db("reembed: storing vector", e))?;
    Ok(())
}
