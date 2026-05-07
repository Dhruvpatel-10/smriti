mod common;
use common::test_db;

use smriti::db::entity;
use smriti::types::{EntityId, EntityKind};

async fn create_entity(
    db: &smriti::db::client::SurrealClient,
    name: &str,
) -> EntityId {
    entity::upsert(
        db,
        name,
        EntityKind::Concept,
        vec![],
        serde_json::json!({}),
        None,
        &[],
    )
    .await
    .expect("entity upsert failed")
}

#[tokio::test]
async fn archive_marks_entity_as_archived() {
    let (db, _dir) = test_db().await;
    let id = create_entity(&db, "ArchiveTestEntity").await;

    let archived = entity::archive(&db, &id, Some("test archival"))
        .await
        .expect("archive failed");
    assert!(archived, "first archive call should return true");

    // Verify state in DB directly (bypass the archived_at IS NONE filter).
    let mut resp = db
        .query("SELECT archived_at, archive_reason FROM entity WHERE record::id(id) = $id")
        .bind(("id", id.as_str().to_string()))
        .await
        .expect("query failed");
    let rows: Vec<serde_json::Value> = resp.take(0usize).unwrap_or_default();
    assert_eq!(rows.len(), 1, "expected one entity row");
    let row = &rows[0];
    assert!(
        !row["archived_at"].is_null(),
        "archived_at should be set after archive()"
    );
    assert_eq!(
        row["archive_reason"].as_str().unwrap_or(""),
        "test archival"
    );
}

#[tokio::test]
async fn archived_entity_hidden_from_find() {
    let (db, _dir) = test_db().await;
    let id = create_entity(&db, "HiddenAfterDeleteEntity").await;

    // Entity should be findable before archival.
    let before = entity::find(&db, "HiddenAfterDeleteEntity", None, None)
        .await
        .expect("find before archive failed");
    assert!(!before.is_empty(), "entity should be visible before archive");

    entity::archive(&db, &id, None)
        .await
        .expect("archive failed");

    // Entity should be invisible after archival.
    let after = entity::find(&db, "HiddenAfterDeleteEntity", None, None)
        .await
        .expect("find after archive failed");
    assert!(after.is_empty(), "archived entity should be hidden from find()");
}
