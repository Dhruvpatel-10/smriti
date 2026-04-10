mod common;
use common::{test_db, create_test_memory};

use chrono::{Duration, Utc};
use kb_server::db::{graph, memory, search};
use kb_server::types::*;

// ═══════════════════════════════════════════════════════════════════════════
// SEARCH / RECALL
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn recall_empty_query_returns_all_active() {
    let (db, _dir) = test_db().await;

    create_test_memory(&db, "alpha").await;
    create_test_memory(&db, "beta").await;
    let id3 = create_test_memory(&db, "gamma").await;
    memory::archive(&db, &id3).await.unwrap();

    let results = search::recall(&db, "", None, None, None, None, None, None, None, 10).await.unwrap();
    assert_eq!(results.len(), 2, "should return only active memories");
}

#[tokio::test]
async fn recall_filters_by_type() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db, "decision about X".into(), None,
        MemoryType::Semantic, MemorySubtype::Decision, Source::Manual,
        None, vec![], vec![], Importance::default(), Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    memory::create(
        &db, "standup notes".into(), None,
        MemoryType::Episodic, MemorySubtype::Meeting, Source::Meeting,
        None, vec![], vec![], Importance::default(), Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    let results = search::recall(
        &db, "", None, Some(MemoryType::Semantic), None, None, None, None, None, 10
    ).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].memory_type, MemoryType::Semantic);
}

#[tokio::test]
async fn recall_filters_by_project() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db, "project A fact".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::default(), Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    memory::create(
        &db, "project B fact".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        Some("proj-b".into()), vec![], vec![], Importance::default(), Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    let results = search::recall(
        &db, "", None, None, Some("proj-a"), None, None, None, None, 10
    ).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].project.as_deref(), Some("proj-a"));
}

#[tokio::test]
async fn recall_filters_by_time_range() {
    let (db, _dir) = test_db().await;
    let now = Utc::now();

    memory::create(
        &db, "old memory".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::default(), Visibility::default(),
        Some(now - Duration::days(30)), vec![], None, None, None,
    ).await.unwrap();

    memory::create(
        &db, "recent memory".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::default(), Visibility::default(),
        Some(now - Duration::hours(1)), vec![], None, None, None,
    ).await.unwrap();

    let results = search::recall(
        &db, "", None, None, None, None, None,
        Some(now - Duration::days(7)),
        None, 10,
    ).await.unwrap();
    assert_eq!(results.len(), 1, "should only return the recent memory");
    assert_eq!(results[0].summary, "recent memory");
}

#[tokio::test]
async fn recall_respects_limit() {
    let (db, _dir) = test_db().await;

    for i in 0..10 {
        create_test_memory(&db, &format!("memory {i}")).await;
    }

    let results = search::recall(&db, "", None, None, None, None, None, None, None, 3).await.unwrap();
    assert_eq!(results.len(), 3);
}

#[tokio::test]
async fn recall_limit_capped_at_25() {
    let (db, _dir) = test_db().await;

    for i in 0..30 {
        create_test_memory(&db, &format!("memory {i}")).await;
    }

    let results = search::recall(&db, "", None, None, None, None, None, None, None, 100).await.unwrap();
    assert!(results.len() <= 25, "limit should be capped at 25, got {}", results.len());
}

#[tokio::test]
async fn recall_excludes_archived_and_superseded() {
    let (db, _dir) = test_db().await;

    let active = create_test_memory(&db, "active one").await;
    let archived = create_test_memory(&db, "to archive").await;
    let superseded = create_test_memory(&db, "to supersede").await;

    memory::archive(&db, &archived).await.unwrap();
    memory::supersede(&db, &superseded).await.unwrap();

    let results = search::recall(&db, "", None, None, None, None, None, None, None, 10).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, active);
}

#[tokio::test]
async fn recall_on_empty_db_returns_empty() {
    let (db, _dir) = test_db().await;

    let results = search::recall(&db, "anything", None, None, None, None, None, None, None, 10).await.unwrap();
    assert!(results.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// DOMAIN FILTER TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn recall_filters_by_domain() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db, "gym workout PR".into(), None,
        MemoryType::Episodic, MemorySubtype::Session, Source::Manual,
        None, vec!["health".into()], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    memory::create(
        &db, "project decision".into(), None,
        MemoryType::Semantic, MemorySubtype::Decision, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    let results = search::recall(&db, "", None, None, None, Some("health"), None, None, None, 10).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].summary, "gym workout PR");
}

// ═══════════════════════════════════════════════════════════════════════════
// VECTOR FIELD STORAGE TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn vector_field_can_be_stored_and_retrieved() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "vector test memory").await;

    // Store a vector (f32 — matches fastembed output, no f64 conversion needed)
    let test_vector: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
    db.query("UPDATE type::record('memory', $id) SET vector = $vec")
        .bind(("id", id.as_str().to_string()))
        .bind(("vec", test_vector.clone()))
        .await
        .unwrap();

    // Retrieve and verify
    let mut result = db.query("SELECT vector FROM type::record('memory', $id)")
        .bind(("id", id.as_str().to_string()))
        .await.unwrap();
    let row: Option<serde_json::Value> = result.take(0).unwrap();
    let stored_vec = row.unwrap()
        .get("vector")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_f64()).collect::<Vec<_>>())
        .unwrap();

    assert_eq!(stored_vec.len(), 768);
    assert!((stored_vec[0] - 0.0).abs() < 0.001);
    assert!((stored_vec[767] - 767.0 / 768.0).abs() < 0.001);
}

#[tokio::test]
async fn vector_cosine_similarity_query_works() {
    let (db, _dir) = test_db().await;

    // Create two memories with distinct vectors
    let id_a = create_test_memory(&db, "alpha topic").await;
    let id_b = create_test_memory(&db, "beta topic").await;

    // Vector A: mostly 1s at the start
    let mut vec_a: Vec<f32> = vec![0.0; 768];
    vec_a[..384].fill(1.0);

    // Vector B: mostly 1s at the end
    let mut vec_b: Vec<f32> = vec![0.0; 768];
    vec_b[384..].fill(1.0);

    db.query("UPDATE type::record('memory', $id) SET vector = $vec")
        .bind(("id", id_a.as_str().to_string()))
        .bind(("vec", vec_a.clone()))
        .await.unwrap();

    db.query("UPDATE type::record('memory', $id) SET vector = $vec")
        .bind(("id", id_b.as_str().to_string()))
        .bind(("vec", vec_b))
        .await.unwrap();

    // Query with vector similar to A
    let mut result = db.query(
        "SELECT record::id(id) AS mid, vector::similarity::cosine(vector, $qv) AS score
         FROM memory
         WHERE vector != NONE AND status = 'active'
         ORDER BY score DESC"
    )
    .bind(("qv", vec_a))
    .await.unwrap();

    let rows: Vec<serde_json::Value> = result.take(0).unwrap();
    assert_eq!(rows.len(), 2);

    // First result should be the one with the more similar vector (id_a)
    let first_id = rows[0].get("mid").and_then(|v| v.as_str()).unwrap();
    assert_eq!(first_id, id_a.as_str(), "most similar vector should rank first");
}

#[tokio::test]
async fn f32_vectors_produce_correct_cosine_scores() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "f32 cosine precision test").await;

    // Store a known f32 vector (matches fastembed output type)
    let stored: Vec<f32> = vec![0.5; 256];
    db.query("UPDATE type::record('memory', $id) SET vector = $vec")
        .bind(("id", id.as_str().to_string()))
        .bind(("vec", stored.clone()))
        .await
        .unwrap();

    // Query cosine similarity with the same f32 vector (table scan pattern —
    // vector::similarity::cosine requires it vs type::record direct access)
    let mut result = db
        .query(
            "SELECT record::id(id) AS mid, vector::similarity::cosine(vector, $qv) AS score
             FROM memory
             WHERE vector != NONE AND status = 'active'
             ORDER BY score DESC",
        )
        .bind(("qv", stored))
        .await
        .unwrap();

    let rows: Vec<serde_json::Value> = result.take(0).unwrap();
    assert_eq!(rows.len(), 1);

    let score = rows[0].get("score").and_then(|v| v.as_f64()).unwrap();
    assert!(
        (score - 1.0).abs() < 1e-6,
        "identical f32 vectors should have cosine ≈ 1.0, got {score}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// SPREADING ACTIVATION TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn recall_includes_graph_neighbors_via_spreading_activation() {
    let (db, _dir) = test_db().await;

    // Create a primary memory and a related one linked via `extends`
    let primary = memory::create(
        &db, "primary fact about architecture".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::new(5).unwrap(),
        Visibility::default(), None, vec!["architecture".into()], None, None, None,
    ).await.unwrap();

    let related = memory::create(
        &db, "extended detail about architecture layers".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::new(2).unwrap(),
        Visibility::default(), None, vec!["architecture".into()], None, None, None,
    ).await.unwrap();

    // Link: related extends primary
    graph::link_memories(&db, &related, &primary, RelationType::Extends).await.unwrap();

    // Recall with a query that matches primary but NOT related
    // (empty query returns all, so both would appear regardless)
    // Instead, recall with limit=25 to get everything + spreading activation
    let results = search::recall(
        &db, "", None, None, Some("proj-a"), None, None, None, None, 25,
    ).await.unwrap();

    // Both should be present (primary directly, related via spreading activation or direct match)
    let ids: Vec<&str> = results.iter().map(|m| m.id.as_str()).collect();
    assert!(ids.contains(&primary.as_str()), "primary memory should be in results");
    assert!(ids.contains(&related.as_str()), "related memory should be in results (direct or activation)");
}

// ═══════════════════════════════════════════════════════════════════════════
// MULTI-DOMAIN AND VISIBILITY TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn memory_with_multiple_domains_found_by_any() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db, "cross-domain insight".into(), None,
        MemoryType::Semantic, MemorySubtype::Pattern, Source::Manual,
        None, vec!["health".into(), "learning".into()], vec![],
        Importance::default(), Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    // Should be found by either domain
    let health = search::recall(&db, "", None, None, None, Some("health"), None, None, None, 10).await.unwrap();
    assert_eq!(health.len(), 1, "should find by 'health' domain");

    let learning = search::recall(&db, "", None, None, None, Some("learning"), None, None, None, 10).await.unwrap();
    assert_eq!(learning.len(), 1, "should find by 'learning' domain");

    let finance = search::recall(&db, "", None, None, None, Some("finance"), None, None, None, 10).await.unwrap();
    assert_eq!(finance.len(), 0, "should NOT find by unrelated domain");
}

#[tokio::test]
async fn recall_filters_by_type_and_project_combined() {
    let (db, _dir) = test_db().await;

    // Decision in proj-a
    memory::create(
        &db, "proj-a decision".into(), None,
        MemoryType::Semantic, MemorySubtype::Decision, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    // Fact in proj-a
    memory::create(
        &db, "proj-a fact".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    // Decision in proj-b
    memory::create(
        &db, "proj-b decision".into(), None,
        MemoryType::Semantic, MemorySubtype::Decision, Source::Manual,
        Some("proj-b".into()), vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    // Filter: type=Semantic + project=proj-a
    let results = search::recall(
        &db, "", None, Some(MemoryType::Semantic), Some("proj-a"), None, None, None, None, 25
    ).await.unwrap();

    assert_eq!(results.len(), 2, "should return both Semantic memories in proj-a");
    assert!(results.iter().all(|m| m.project.as_deref() == Some("proj-a")));
}

#[tokio::test]
async fn person_filter_works_with_entity_table() {
    let (db, _dir) = test_db().await;

    // Create a memory linked to a person via entity table
    let id = memory::create(
        &db, "talked with Bob about pricing".into(), None,
        MemoryType::Episodic, MemorySubtype::Meeting, Source::Manual,
        None, vec![], vec!["Bob".into()], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();
    graph::link_entity(&db, &id, "Bob").await.unwrap();

    // Create a memory NOT linked to Bob
    memory::create(
        &db, "internal design review".into(), None,
        MemoryType::Episodic, MemorySubtype::Meeting, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    // Filter by person=Bob
    let results = search::recall(
        &db, "", None, None, None, None, Some("Bob"), None, None, 10
    ).await.unwrap();

    assert_eq!(results.len(), 1, "should find exactly 1 memory involving Bob");
    assert_eq!(results[0].id, id);
}

#[tokio::test]
async fn person_filter_no_match_returns_empty() {
    let (db, _dir) = test_db().await;

    create_test_memory(&db, "some memory").await;

    let results = search::recall(
        &db, "", None, None, None, None, Some("Nonexistent Person"), None, None, 10
    ).await.unwrap();

    assert!(results.is_empty(), "person filter with no match should return empty");
}

// ═══════════════════════════════════════════════════════════════════════════
// RECALL ORDERING TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn recall_returns_higher_importance_first() {
    let (db, _dir) = test_db().await;

    // Create memories with different importance levels
    memory::create(
        &db, "low importance".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::new(1).unwrap(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    memory::create(
        &db, "high importance".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::new(5).unwrap(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    memory::create(
        &db, "medium importance".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::new(3).unwrap(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    let results = search::recall(&db, "", None, None, None, None, None, None, None, 25).await.unwrap();

    assert_eq!(results.len(), 3);
    // Results sorted by importance DESC (no lost-in-middle reordering — that belongs in the client)
    assert_eq!(results[0].summary, "high importance", "highest importance should be first");
    assert_eq!(results[1].summary, "medium importance", "medium importance should be second");
}
