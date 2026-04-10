use tracing::instrument;

use crate::db::client::SurrealClient;
use crate::error::{KbError, Result};
use crate::types::MemoryId;

/// Deduplication thresholds (tuned post-DA review)
const REJECT_THRESHOLD: f64 = 0.92;
const WARN_THRESHOLD: f64 = 0.82;

/// Result of a dedup check against existing memories.
#[derive(Debug)]
pub enum DedupResult {
    /// No duplicates found — safe to store.
    Clean,
    /// Near-duplicate detected (similarity 0.85–0.95). Stored but flagged.
    Warning {
        similar_id: String,
        similarity: f64,
    },
    /// Duplicate detected (similarity > 0.95). Reject the write.
    Duplicate {
        existing_id: String,
        similarity: f64,
    },
}

/// Check if a new memory's embedding is a duplicate of an existing memory.
///
/// Uses brute-force cosine similarity scan. Fast enough for personal KB scale
/// (<100k records). Returns the most similar existing memory.
#[instrument(skip(db, embedding))]
pub async fn check_duplicate(
    db: &SurrealClient,
    embedding: &[f32],
) -> Result<DedupResult> {
    let mut result = db
        .query(
            "SELECT record::id(id) AS mem_id, vector::similarity::cosine(vector, $qv) AS score
             FROM memory
             WHERE status = 'active' AND vector != NONE
             ORDER BY score DESC
             LIMIT 1",
        )
        .bind(("qv", embedding.to_vec()))
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

/// Find similar memories above a given threshold (for auto-linking).
/// Returns up to `limit` memory IDs with their similarity scores.
#[instrument(skip(db, embedding))]
pub async fn find_similar(
    db: &SurrealClient,
    embedding: &[f32],
    exclude_id: &MemoryId,
    threshold: f64,
    limit: usize,
) -> Result<Vec<(String, f64, Option<String>)>> {
    let limit_i64 = limit as i64;

    let mut result = db
        .query(
            "SELECT record::id(id) AS mem_id,
                    project,
                    vector::similarity::cosine(vector, $qv) AS score
             FROM memory
             WHERE status = 'active'
               AND vector != NONE
               AND record::id(id) != $exclude
             ORDER BY score DESC
             LIMIT $limit",
        )
        .bind(("qv", embedding.to_vec()))
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
