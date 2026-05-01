mod common;
use common::test_db;

use smriti::db::memory as db_memory;
use smriti::db::typed_relation;
use smriti::types::{Channel, MemoryId, MemorySubtype, MemoryType, Modality, RelationKind};
use surrealdb::types::RecordId;

async fn create_memory(db: &smriti::db::client::SurrealClient, summary: &str) -> MemoryId {
    db_memory::create(
        db,
        db_memory::MemoryDraft::new(
            summary.to_string(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        ),
    )
    .await
    .expect("test memory creation failed")
}

#[tokio::test]
async fn retract_marks_edge_as_retracted() {
    let (db, _dir) = test_db().await;
    let from = create_memory(&db, "causes memory for retract test").await;
    let to = create_memory(&db, "effect memory for retract test").await;

    // Create the edge first.
    typed_relation::upsert(&db, &from, &to, RelationKind::Causes, 0.9, "test/v1", &from)
        .await
        .expect("upsert failed");

    // Retract it.
    let retracted =
        typed_relation::retract(&db, &from, &to, RelationKind::Causes, Some("test reason"))
            .await
            .expect("retract failed");
    assert!(retracted, "retract should return true on first call");

    // Verify the DB state: retracted_at IS NOT NONE and reason matches.
    let mut resp = db
        .query("SELECT retracted_at, retraction_reason FROM typed_relation WHERE in = $from AND out = $to AND kind = $kind")
        .bind(("from", RecordId::new("memory", from.as_str())))
        .bind(("to", RecordId::new("memory", to.as_str())))
        .bind(("kind", "causes".to_string()))
        .await
        .expect("query failed");
    let rows: Vec<serde_json::Value> = resp.take(0usize).unwrap_or_default();
    assert_eq!(rows.len(), 1, "expected exactly one edge");
    let row = &rows[0];
    assert!(
        !row["retracted_at"].is_null(),
        "retracted_at should be set"
    );
    assert_eq!(
        row["retraction_reason"].as_str().unwrap_or(""),
        "test reason"
    );
}

#[tokio::test]
async fn retract_is_idempotent() {
    let (db, _dir) = test_db().await;
    let from = create_memory(&db, "idempotent retract from memory").await;
    let to = create_memory(&db, "idempotent retract to memory").await;

    typed_relation::upsert(&db, &from, &to, RelationKind::Enables, 0.8, "test/v1", &from)
        .await
        .expect("upsert failed");

    // First retract — should succeed.
    let first =
        typed_relation::retract(&db, &from, &to, RelationKind::Enables, None)
            .await
            .expect("first retract failed");
    assert!(first, "first retract should return true");

    // Second retract — already retracted, should return false.
    let second =
        typed_relation::retract(&db, &from, &to, RelationKind::Enables, None)
            .await
            .expect("second retract failed");
    assert!(!second, "second retract should return false (already retracted)");
}
