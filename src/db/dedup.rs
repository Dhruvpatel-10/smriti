use surrealdb::types::RecordId;
use tracing::instrument;

use crate::db::client::SurrealClient;
use crate::db::BITEMPORAL_ACTIVE;
use crate::error::{KbError, Result};
use crate::types::{AgentId, MemoryId};

/// Deduplication thresholds (tuned post-DA review)
pub const REJECT_THRESHOLD: f64 = 0.92;
pub const WARN_THRESHOLD: f64 = 0.82;

/// Bench/test escape hatch: when `SMRITI_DEDUP_DISABLE=1`, all dedup checks
/// short-circuit to `Clean`. Lets synthetic benchmarks ingest semantically
/// similar corpora without server-side rejection. Never set in production.
fn dedup_disabled() -> bool {
    std::env::var("SMRITI_DEDUP_DISABLE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Result of a dedup check against existing memories.
#[derive(Debug)]
pub enum DedupResult {
    /// No duplicates found — safe to store.
    Clean,
    /// Near-duplicate detected (similarity 0.82–0.92). Stored but flagged.
    Warning {
        similar_id: String,
        similarity: f64,
    },
    /// Duplicate detected (similarity > 0.92). Reject the write.
    Duplicate {
        existing_id: String,
        similarity: f64,
    },
}

/// Check if a new memory's embedding is a duplicate of an existing memory.
///
/// Uses brute-force cosine similarity scan. Fast enough for personal KB scale
/// (<100k records). Returns the most similar existing memory.
///
/// Production write path uses `memory::create_with_dedup_atomic` (atomic dedup + create).
/// This unscoped variant is used by tests exercising the dedup thresholds directly.
#[instrument(skip(db, embedding))]
pub async fn check_duplicate(
    db: &SurrealClient,
    embedding: Vec<f32>,
) -> Result<DedupResult> {
    if dedup_disabled() {
        return Ok(DedupResult::Clean);
    }
    let knn = crate::db::vector_index::candidate_predicate(8);
    let mut result = db
        .query(format!(
            "SELECT record::id(id) AS mem_id, vector::similarity::cosine(vector, $qv) AS score
             FROM memory
             WHERE {BITEMPORAL_ACTIVE} AND {knn}
             ORDER BY score DESC
             LIMIT 1"
        ))
        .bind(("qv", embedding))
        .await
        .map_err(|e| KbError::db("dedup cosine search", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing dedup results", e))?;

    if let Some(row) = rows.first() {
        let score = row.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let mem_id = row
            .get("mem_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if score > REJECT_THRESHOLD && !mem_id.is_empty() {
            return Ok(DedupResult::Duplicate {
                existing_id: mem_id,
                similarity: score,
            });
        }
        if score > WARN_THRESHOLD && !mem_id.is_empty() {
            return Ok(DedupResult::Warning {
                similar_id: mem_id,
                similarity: score,
            });
        }
    }

    Ok(DedupResult::Clean)
}

/// Check if a new memory's embedding is a duplicate within a single author bucket.
///
/// `author`: `Some(id)` → only compare against memories authored by that agent;
/// `None` → only compare against user-authored memories (`author IS NONE`).
/// This prevents cross-author false rejections.
///
/// Used directly by tests for per-bucket dedup checks. Production write path
/// uses `memory::create_with_dedup_atomic` which inlines an equivalent scoped check.
#[instrument(skip(db, embedding))]
pub async fn check_duplicate_scoped(
    db: &SurrealClient,
    embedding: Vec<f32>,
    author: Option<&AgentId>,
) -> Result<DedupResult> {
    if dedup_disabled() {
        return Ok(DedupResult::Clean);
    }
    let knn = crate::db::vector_index::candidate_predicate(8);
    let rows = if let Some(agent_id) = author {
        let rid = RecordId::new("agent", agent_id.as_str());
        let mut result = db
            .query(format!(
                "SELECT record::id(id) AS mem_id, vector::similarity::cosine(vector, $qv) AS score
                 FROM memory
                 WHERE {BITEMPORAL_ACTIVE} AND {knn} AND author = $author_id
                 ORDER BY score DESC
                 LIMIT 1"
            ))
            .bind(("qv", embedding))
            .bind(("author_id", rid))
            .await
            .map_err(|e| KbError::db("scoped dedup cosine search (agent)", e))?;
        let r: Vec<serde_json::Value> = result
            .take(0)
            .map_err(|e| KbError::db("deserializing scoped dedup results (agent)", e))?;
        r
    } else {
        let mut result = db
            .query(format!(
                "SELECT record::id(id) AS mem_id, vector::similarity::cosine(vector, $qv) AS score
                 FROM memory
                 WHERE {BITEMPORAL_ACTIVE} AND {knn} AND author IS NONE
                 ORDER BY score DESC
                 LIMIT 1"
            ))
            .bind(("qv", embedding))
            .await
            .map_err(|e| KbError::db("scoped dedup cosine search (user)", e))?;
        let r: Vec<serde_json::Value> = result
            .take(0)
            .map_err(|e| KbError::db("deserializing scoped dedup results (user)", e))?;
        r
    };

    if let Some(row) = rows.first() {
        let score = row.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let mem_id = row
            .get("mem_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if score > REJECT_THRESHOLD && !mem_id.is_empty() {
            return Ok(DedupResult::Duplicate {
                existing_id: mem_id,
                similarity: score,
            });
        }
        if score > WARN_THRESHOLD && !mem_id.is_empty() {
            return Ok(DedupResult::Warning {
                similar_id: mem_id,
                similarity: score,
            });
        }
    }

    Ok(DedupResult::Clean)
}

/// Find similar memories above a given threshold (for auto-linking).
/// Returns up to `limit` memory IDs with their similarity scores.
#[instrument(skip(db, embedding))]
pub async fn find_similar(
    db: &SurrealClient,
    embedding: Vec<f32>,
    exclude_id: &MemoryId,
    threshold: f64,
    limit: usize,
) -> Result<Vec<(String, f64, Option<String>)>> {
    let limit_i64 = limit as i64;
    let knn = crate::db::vector_index::candidate_predicate(limit * 2 + 8);

    let mut result = db
        .query(format!(
            "SELECT record::id(id) AS mem_id,
                    project,
                    vector::similarity::cosine(vector, $qv) AS score
             FROM memory
             WHERE {BITEMPORAL_ACTIVE}
               AND {knn}
               AND record::id(id) != $exclude
             ORDER BY score DESC
             LIMIT $limit"
        ))
        .bind(("qv", embedding))
        .bind(("exclude", exclude_id.as_str().to_string()))
        .bind(("limit", limit_i64))
        .await
        .map_err(|e| KbError::db("find_similar cosine search", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing find_similar results", e))?;

    let results: Vec<(String, f64, Option<String>)> = rows
        .into_iter()
        .filter_map(|row| {
            let score = row.get("score").and_then(|v| v.as_f64())?;
            let mem_id = row.get("mem_id").and_then(|v| v.as_str())?.to_string();
            let project = row.get("project").and_then(|v| v.as_str()).map(String::from);
            if score >= threshold && !mem_id.is_empty() {
                Some((mem_id, score, project))
            } else {
                None
            }
        })
        .collect();

    Ok(results)
}
