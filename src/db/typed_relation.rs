//! Persistence layer for `typed_relation` edges (LLM-extracted typed relations).
//!
//! See `migrations.rs` MIGRATION_012 for schema. The edge carries:
//!   in (`record<memory>`), out (`record<memory>`), kind (string, controlled vocab),
//!   confidence (float \[0,1\]), extracted_at, extractor_version, source_memory_id,
//!   retracted_at, retraction_reason.

use surrealdb::types::RecordId;
use tracing::{instrument, warn};

use crate::db::client::SurrealClient;
use crate::error::{KbError, Result};
use crate::types::{MemoryId, RelationKind};

/// Persist a single typed relation. Idempotent on (in, out, kind).
/// Returns Ok(true) if a new edge was created, Ok(false) if it already existed
/// or the target memory doesn't exist.
#[instrument(skip(db))]
pub async fn upsert(
    db: &SurrealClient,
    from: &MemoryId,
    to: &MemoryId,
    kind: RelationKind,
    confidence: f32,
    extractor_version: &str,
    source_memory_id: &MemoryId,
) -> Result<bool> {
    let from_rid = RecordId::new("memory", from.as_str());
    let to_rid = RecordId::new("memory", to.as_str());
    let source_rid = RecordId::new("memory", source_memory_id.as_str());

    // Multi-statement query: check existence then conditionally RELATE.
    // `typed_relation` is a static identifier (not user input), so interpolating
    // the table name in RELATE is safe — SurrealDB v3 doesn't support bind
    // variables in the middle slot of RELATE.
    let mut response = db
        .query(
            "LET $exists = (SELECT * FROM typed_relation WHERE in = $from AND out = $to AND kind = $kind LIMIT 1);
             LET $target = (SELECT id FROM type::record('memory', $to_str) LIMIT 1);
             IF $target = [] {
                 RETURN { skipped: 'target_missing' };
             } ELSE IF $exists != [] {
                 RETURN { skipped: 'duplicate' };
             } ELSE {
                 RELATE $from -> typed_relation -> $to
                     SET kind = $kind,
                         confidence = $confidence,
                         extractor_version = $version,
                         source_memory_id = $source;
                 RETURN { created: true };
             };",
        )
        .bind(("from", from_rid))
        .bind(("to", to_rid))
        .bind(("to_str", to.as_str().to_string()))
        .bind(("kind", kind.to_string()))
        .bind(("confidence", confidence))
        .bind(("version", extractor_version.to_string()))
        .bind(("source", source_rid))
        .await
        .map_err(|e| KbError::db(format!("upserting typed_relation {from} -[{kind}]-> {to}"), e))?;

    // Consume the first two LET statements.
    let _: Vec<serde_json::Value> = response.take(0usize).unwrap_or_default();
    let _: Vec<serde_json::Value> = response.take(1usize).unwrap_or_default();

    // The IF/ELSE result is at index 2.
    let result: Option<serde_json::Value> = response.take(2usize).unwrap_or_default();

    let obj = match &result {
        Some(v) => v,
        None => return Ok(false),
    };

    if let Some(skipped) = obj.get("skipped").and_then(|v| v.as_str()) {
        if skipped == "target_missing" {
            warn!(
                target: "typed_relation",
                from = %from,
                to = %to,
                "upsert skipped: target memory not found (possible hallucinated ID)"
            );
        }
        // duplicate is expected — no warn
        return Ok(false);
    }

    if obj.get("created").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Ok(true);
    }

    Ok(false)
}

/// Soft-retract a typed_relation edge identified by (from, to, kind).
///
/// Idempotent: returns Ok(false) if the edge doesn't exist or is already
/// retracted. Returns Ok(true) if the edge was retracted by this call.
#[instrument(skip(db))]
pub async fn retract(
    db: &SurrealClient,
    from: &MemoryId,
    to: &MemoryId,
    kind: RelationKind,
    reason: Option<&str>,
) -> Result<bool> {
    let from_rid = RecordId::new("memory", from.as_str());
    let to_rid = RecordId::new("memory", to.as_str());

    let mut response = db
        .query(
            "UPDATE typed_relation
             SET retracted_at = time::now(),
                 retraction_reason = $reason
             WHERE in = $from AND out = $to AND kind = $kind AND retracted_at IS NONE",
        )
        .bind(("from", from_rid))
        .bind(("to", to_rid))
        .bind(("kind", kind.to_string()))
        .bind(("reason", reason.map(str::to_string)))
        .await
        .map_err(|e| KbError::db(format!("retracting typed_relation {from} -[{kind}]-> {to}"), e))?;

    let updated: Vec<serde_json::Value> = response.take(0usize).unwrap_or_default();
    Ok(!updated.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::client;
    use crate::db::memory as db_memory;
    use crate::types::{MemorySubtype, MemoryType};

    async fn test_db() -> (client::SurrealClient, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let db = client::connect(dir.path()).await.expect("failed to connect");
        (db, dir)
    }

    async fn create_memory(db: &client::SurrealClient, summary: &str) -> MemoryId {
        db_memory::create(
            db,
            db_memory::MemoryDraft::new(
                summary.to_string(),
                MemoryType::Semantic,
                MemorySubtype::Fact,
                None,
                None,
            ),
        )
        .await
        .expect("create memory failed")
    }

    #[tokio::test]
    async fn upsert_creates_new_edge() {
        let (db, _dir) = test_db().await;
        let from = create_memory(&db, "first fact").await;
        let to = create_memory(&db, "second fact").await;

        let created = upsert(&db, &from, &to, RelationKind::Causes, 0.9, "test/v1", &from)
            .await
            .expect("upsert failed");
        assert!(created, "first upsert should return true");

        // Second call with same triple — idempotent, returns false.
        let created_again = upsert(&db, &from, &to, RelationKind::Causes, 0.9, "test/v1", &from)
            .await
            .expect("second upsert failed");
        assert!(!created_again, "second upsert should return false (duplicate)");
    }

    #[tokio::test]
    async fn upsert_skips_when_target_missing() {
        let (db, _dir) = test_db().await;
        let from = create_memory(&db, "existing memory").await;
        let ghost = MemoryId::from("mem_doesnotexist999".to_string());

        let created = upsert(&db, &from, &ghost, RelationKind::Mentions, 0.8, "test/v1", &from)
            .await
            .expect("upsert failed");
        assert!(!created, "should skip when target doesn't exist");

        // Verify no edge in DB.
        let mut resp = db
            .query("SELECT * FROM typed_relation WHERE in = $from")
            .bind(("from", surrealdb::types::RecordId::new("memory", from.as_str())))
            .await
            .expect("query failed");
        let edges: Vec<serde_json::Value> = resp.take(0usize).unwrap_or_default();
        assert!(edges.is_empty(), "no edge should exist for missing target");
    }

    #[tokio::test]
    async fn upsert_allows_different_kind_same_pair() {
        let (db, _dir) = test_db().await;
        let from = create_memory(&db, "memory a").await;
        let to = create_memory(&db, "memory b").await;

        let first = upsert(&db, &from, &to, RelationKind::Causes, 0.85, "test/v1", &from)
            .await
            .expect("first upsert");
        assert!(first);

        // Different kind — should create a second edge.
        let second = upsert(&db, &from, &to, RelationKind::Enables, 0.75, "test/v1", &from)
            .await
            .expect("second upsert");
        assert!(second, "different kind should create a new edge");

        let mut resp = db
            .query("SELECT * FROM typed_relation WHERE in = $from")
            .bind(("from", surrealdb::types::RecordId::new("memory", from.as_str())))
            .await
            .expect("query failed");
        let edges: Vec<serde_json::Value> = resp.take(0usize).unwrap_or_default();
        assert_eq!(edges.len(), 2, "expected two edges for different kinds");
    }

    #[tokio::test]
    async fn upsert_persists_all_fields() {
        let (db, _dir) = test_db().await;
        let from = create_memory(&db, "cause memory").await;
        let to = create_memory(&db, "effect memory").await;

        let created = upsert(&db, &from, &to, RelationKind::DependsOn, 0.95, "claude/v42", &from)
            .await
            .expect("upsert failed");
        assert!(created);

        let mut resp = db
            .query("SELECT * FROM typed_relation WHERE in = $from AND out = $to")
            .bind(("from", surrealdb::types::RecordId::new("memory", from.as_str())))
            .bind(("to", surrealdb::types::RecordId::new("memory", to.as_str())))
            .await
            .expect("query failed");
        let edges: Vec<serde_json::Value> = resp.take(0usize).unwrap_or_default();
        assert_eq!(edges.len(), 1);

        let edge = &edges[0];
        assert_eq!(edge["kind"].as_str().unwrap(), "depends_on");
        // confidence is stored as float; serde_json represents it as f64
        let conf = edge["confidence"].as_f64().unwrap();
        assert!((conf - 0.95_f64).abs() < 1e-4, "confidence mismatch: {conf}");
        assert_eq!(edge["extractor_version"].as_str().unwrap(), "claude/v42");
    }
}
