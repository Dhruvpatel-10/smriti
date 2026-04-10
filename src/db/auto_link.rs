use tracing::{instrument, warn};

use crate::db::client::SurrealClient;
use crate::db::dedup;
use crate::db::graph;
use crate::error::{KbError, Result};
use crate::types::{MemoryId, RelationType};

/// Similarity threshold for creating `relates` edges (same project/domain).
const RELATES_THRESHOLD: f64 = 0.75;
/// Similarity threshold for cross-domain `relates` edges (higher bar).
const CROSS_DOMAIN_THRESHOLD: f64 = 0.85;
/// Similarity threshold for creating `extends` edges (same project only).
const EXTENDS_THRESHOLD: f64 = 0.90;
/// Maximum auto-linked similarity edges per memory.
const MAX_SIMILARITY_EDGES: usize = 8;

/// Auto-link a newly stored memory to similar existing memories via graph edges.
///
/// Thresholds (post-DA review):
/// - similarity > 0.90 AND same project → `extends` edge
/// - similarity > 0.75 (same project/domain) or > 0.85 (cross-domain) → `relates` edge
/// - Capped at 8 similarity edges per memory
///
/// Runs async after memory creation. Errors are logged, not propagated.
#[instrument(skip(db, embedding))]
pub async fn auto_link_similar(
    db: &SurrealClient,
    memory_id: &MemoryId,
    memory_project: Option<&str>,
    embedding: &[f32],
) -> Result<u32> {
    // Fetch more candidates than the cap so we can filter by threshold.
    // find_similar returns (id, score, project) — use project directly to
    // avoid N+1 check_same_project queries.
    let similar = dedup::find_similar(db, embedding, memory_id, RELATES_THRESHOLD, 15).await?;

    let mut links_created = 0u32;

    for (similar_id, score, similar_project) in similar {
        if links_created as usize >= MAX_SIMILARITY_EDGES {
            break;
        }

        let same_project = memory_project
            .is_some_and(|proj| similar_project.as_deref() == Some(proj));

        // Determine relation type and whether to create the edge
        let relation = if score >= EXTENDS_THRESHOLD && same_project {
            Some(RelationType::Extends)
        } else if score >= RELATES_THRESHOLD && same_project {
            Some(RelationType::Relates)
        } else if score >= CROSS_DOMAIN_THRESHOLD {
            // Cross-domain links require higher confidence
            Some(RelationType::Relates)
        } else {
            None // Below threshold for cross-domain
        };

        if let Some(rel) = relation {
            let similar_mem_id = MemoryId::from(similar_id.clone());
            match graph::link_memories(db, memory_id, &similar_mem_id, rel).await {
                Ok(()) => links_created += 1,
                Err(e) => warn!(from = %memory_id, to = %similar_id, "auto-link failed: {e}"),
            }
        }
    }

    Ok(links_created)
}

/// Scan a summary for known entity names and create `involves` edges.
///
/// Uses whole-word boundary matching to avoid false positives like
/// "Jane" matching inside "plaintext". Names must be at least 4 chars.
#[instrument(skip(db, summary))]
pub async fn match_known_entities(
    db: &SurrealClient,
    memory_id: &MemoryId,
    summary: &str,
) -> Result<u32> {
    // Let SurrealDB filter entities whose name appears in the summary,
    // instead of loading all entities into Rust. Much faster as entity count grows.
    let mut result = db
        .query(
            "SELECT name FROM entity
             WHERE string::lowercase($summary) CONTAINS string::lowercase(name)
               AND string::len(name) >= 4"
        )
        .bind(("summary", summary.to_string()))
        .await
        .map_err(|e| KbError::db("fetching matching entity names", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing entity names", e))?;

    let lower_summary = summary.to_lowercase();
    let mut links_created = 0u32;

    // DB-side CONTAINS is a substring check; verify word boundaries in Rust
    // on the (much smaller) result set.
    for row in rows {
        if let Some(name) = row.get("name").and_then(|v| v.as_str())
            && word_boundary_match(&lower_summary, &name.to_lowercase())
        {
            match graph::link_entity(db, memory_id, name).await {
                Ok(()) => links_created += 1,
                Err(e) => warn!(id = %memory_id, "entity auto-link failed: {e}"),
            }
        }
    }

    Ok(links_created)
}

/// Check if `needle` appears in `haystack` at a word boundary.
/// A word boundary is a position where the adjacent character is not alphanumeric.
fn word_boundary_match(haystack: &str, needle: &str) -> bool {
    let needle_len = needle.len();
    let haystack_bytes = haystack.as_bytes();

    for (i, _) in haystack.match_indices(needle) {
        // Check left boundary: start of string or non-alphanumeric
        let left_ok = i == 0 || !haystack_bytes[i - 1].is_ascii_alphanumeric();
        // Check right boundary: end of string or non-alphanumeric
        let right_pos = i + needle_len;
        let right_ok = right_pos >= haystack.len() || !haystack_bytes[right_pos].is_ascii_alphanumeric();

        if left_ok && right_ok {
            return true;
        }
    }
    false
}

/// Spawn auto-linking with a pre-computed vector (no double-embed).
/// Combines embedding-based similarity linking + entity name matching.
pub fn spawn_auto_link_with_vector(
    db: SurrealClient,
    memory_id: MemoryId,
    memory_project: Option<String>,
    summary: String,
    vector: Vec<f32>,
) {
    tokio::spawn(async move {
        // Entity name matching (doesn't need embedding)
        match match_known_entities(&db, &memory_id, &summary).await {
            Ok(count) if count > 0 => {
                tracing::info!(id = %memory_id, count, "auto-linked entities");
            }
            Err(e) => warn!(id = %memory_id, "entity matching failed: {e}"),
            _ => {}
        }

        // Embedding-based similarity linking with pre-computed vector
        match auto_link_similar(
            &db,
            &memory_id,
            memory_project.as_deref(),
            &vector,
        )
        .await
        {
            Ok(count) if count > 0 => {
                tracing::info!(id = %memory_id, count, "auto-linked similar memories");
            }
            Err(e) => warn!(id = %memory_id, "auto-link similar failed: {e}"),
            _ => {}
        }
    });
}
