mod common;
use common::{test_db, create_test_memory};

use smriti::db::{entity, graph, memory};
use smriti::types::*;
use surrealdb::types::RecordId;

// ═══════════════════════════════════════════════════════════════════════════
// GRAPH RELATIONS
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn link_memories_creates_relation() {
    let (db, _dir) = test_db().await;

    let id1 = create_test_memory(&db, "first decision").await;
    let id2 = create_test_memory(&db, "updated decision").await;

    graph::link_memories(&db, &id2, &id1, RelationType::Updates)
        .await
        .expect("link failed");
}

#[tokio::test]
async fn link_entity_upserts_and_relates() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "meeting with alice").await;

    graph::link_entity(&db, &id, "Alice").await.expect("first link");
    graph::link_entity(&db, &id, "Alice").await.expect("second link (upsert, should not fail)");
}

#[tokio::test]
async fn find_entities_returns_matches() {
    let (db, _dir) = test_db().await;

    let id1 = create_test_memory(&db, "meeting 1").await;
    let id2 = create_test_memory(&db, "meeting 2").await;

    graph::link_entity(&db, &id1, "Alice Johnson").await.unwrap();
    graph::link_entity(&db, &id2, "Alice Johnson").await.unwrap();
    graph::link_entity(&db, &id1, "Bob Smith").await.unwrap();

    let results = entity::find(&db, "alice", None, None).await.unwrap();
    assert!(!results.is_empty(), "should find Alice");
    assert!(results.iter().any(|p| p.name == "Alice Johnson"));
}

#[tokio::test]
async fn find_entities_no_match_returns_empty() {
    let (db, _dir) = test_db().await;

    let results = entity::find(&db, "nobody", None, None).await.unwrap();
    assert!(results.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// ENTITY TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn entity_upsert_and_get() {
    let (db, _dir) = test_db().await;
    use smriti::db::entity;

    let id = entity::upsert(
        &db, "Jane Park", "person",
        vec!["investor".into()],
        serde_json::json!({"fund": "Sequoia", "check_size": "$500k"}),
        Some("Met at demo day".into()),
    ).await.expect("upsert failed");

    let ent = entity::get(&db, &id).await.expect("get failed");
    assert_eq!(ent.name, "Jane Park");
    assert_eq!(ent.kind, "person");
    assert_eq!(ent.roles, vec!["investor"]);
    assert!(ent.context.as_deref() == Some("Met at demo day"));
}

#[tokio::test]
async fn entity_upsert_is_idempotent() {
    let (db, _dir) = test_db().await;
    use smriti::db::entity;

    let id1 = entity::upsert(&db, "Alice", "person", vec![], serde_json::json!({}), None).await.unwrap();
    let id2 = entity::upsert(&db, "Alice", "person", vec!["engineer".into()], serde_json::json!({"team": "backend"}), Some("Updated context".into())).await.unwrap();

    // Should be the same entity (name is unique key)
    assert_eq!(id1, id2);

    let ent = entity::get(&db, &id1).await.unwrap();
    assert_eq!(ent.roles, vec!["engineer"]);
    assert_eq!(ent.context.as_deref(), Some("Updated context"));
}

#[tokio::test]
async fn entity_find_by_name() {
    let (db, _dir) = test_db().await;
    use smriti::db::entity;

    entity::upsert(&db, "Alice Johnson", "person", vec!["engineer".into()], serde_json::json!({}), None).await.unwrap();
    entity::upsert(&db, "Bob Smith", "person", vec!["designer".into()], serde_json::json!({}), None).await.unwrap();
    entity::upsert(&db, "Acme Corp", "company", vec!["customer".into()], serde_json::json!({}), None).await.unwrap();

    let results = entity::find(&db, "alice", None, None).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "Alice Johnson");

    let results = entity::find(&db, "alice", Some("company"), None).await.unwrap();
    assert!(results.is_empty(), "Alice is a person, not a company");
}

#[tokio::test]
async fn entity_link_and_find_via_graph() {
    let (db, _dir) = test_db().await;
    use smriti::db::entity;

    let mem_id = create_test_memory(&db, "meeting with investor").await;
    graph::link_entity(&db, &mem_id, "Jane Investor").await.unwrap();

    let results = entity::find(&db, "jane", None, None).await.unwrap();
    assert!(!results.is_empty());
    assert!(results[0].memory_count >= 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// IDEMPOTENT GRAPH EDGES
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn duplicate_relate_edges_are_prevented() {
    let (db, _dir) = test_db().await;

    let a = create_test_memory(&db, "memory A").await;
    let b = create_test_memory(&db, "memory B").await;

    // Link twice with the same relation
    graph::link_memories(&db, &a, &b, RelationType::Relates).await.unwrap();
    graph::link_memories(&db, &a, &b, RelationType::Relates).await.unwrap();

    // Count edges — should be exactly 1
    let mut result = db.query("SELECT count() AS total FROM relates WHERE in = $from AND out = $to GROUP ALL")
        .bind(("from", RecordId::new("memory", a.as_str())))
        .bind(("to", RecordId::new("memory", b.as_str())))
        .await.unwrap();
    let row: Option<serde_json::Value> = result.take(0).unwrap();
    let count = row.and_then(|v| v.get("total").and_then(|t| t.as_u64())).unwrap_or(0);

    assert_eq!(count, 1, "duplicate RELATE should be idempotent, got {count} edges");
}

#[tokio::test]
async fn duplicate_entity_link_is_idempotent() {
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db, "Met with Alice".into(), None,
        MemoryType::Episodic, MemorySubtype::Meeting, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    // Link same entity twice
    graph::link_entity(&db, &id, "Alice").await.unwrap();
    graph::link_entity(&db, &id, "Alice").await.unwrap();

    // Count involves edges for this memory → Alice entity
    let mut result = db.query(
        "SELECT count() AS total FROM involves WHERE in = $mem GROUP ALL"
    )
    .bind(("mem", RecordId::new("memory", id.as_str())))
    .await.unwrap();
    let row: Option<serde_json::Value> = result.take(0).unwrap();
    let count = row.and_then(|v| v.get("total").and_then(|t| t.as_u64())).unwrap_or(0);

    assert_eq!(count, 1, "duplicate entity link should be idempotent, got {count}");
}
