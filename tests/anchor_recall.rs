mod common;
use common::test_db;

use smriti::db::{auto_link, entity, memory as db_memory, search};
use smriti::db::search::AuthorFilter;
use smriti::types::{Channel, EntityKind, MemorySubtype, MemoryType, Modality, Visibility};

/// Verify that a memory with anchor_entities set is found by BM25 when
/// the query matches an anchor term that does not appear in the summary.
///
/// This exercises the `anchor_entities @@ $query` extension to the BM25 stage
/// without requiring a live LLM sidecar — anchors are injected directly via
/// `db::memory::set_anchors`.
#[tokio::test]
async fn anchor_entities_found_by_bm25_recall() {
    let (db, _dir) = test_db().await;

    // Create a memory whose summary contains "Sarah" and "gRPC interceptor rollout"
    // but not the synonym "grpc" by itself.
    let id = db_memory::create(
        &db,
        db_memory::MemoryDraft::new(
            "Sarah delivered the gRPC interceptor rollout".to_string(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        ),
    )
    .await
    .expect("create memory");

    // Inject anchors directly (simulating what the sidecar would write).
    db_memory::set_anchors(&db, &id, &["Sarah".to_string(), "gRPC interceptor rollout".to_string()])
        .await
        .expect("set_anchors");

    // Query using "Sarah" which IS in the summary — should hit on summary @@ too.
    let results = search::recall(
        &db,
        "Sarah promise gRPC",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        10,
        &AuthorFilter::None,
        0.0,
        &Default::default(),
        &Default::default(),
        smriti::types::RerankerPolicy::default(),
        None,
        None,
    )
    .await
    .expect("recall");

    let found = results.iter().any(|m| m.id == id);
    assert!(
        found,
        "memory with anchor 'Sarah' + 'gRPC interceptor rollout' should appear in top-10 for 'Sarah promise gRPC'"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// ANCHOR CANONICALIZATION (auto_link::match_known_entities)
// ═══════════════════════════════════════════════════════════════════════════

/// `match_known_entities` stores the canonical entity name from the FTS-matched
/// row — not the raw extraction token — into `memory.anchor_entities`.
///
/// Fixture: entity "Robert Smith" is upserted; a memory whose summary contains
/// "Robert Smith" (the full canonical name as a token that FTS matches) is
/// created, then `match_known_entities` is called directly.  We assert that
/// `anchor_entities` contains the canonical "Robert Smith" string and not some
/// other form.
#[tokio::test]
async fn match_known_entities_stores_canonical_name() {
    let (db, _dir) = test_db().await;

    // Pre-existing entity with canonical name.
    entity::upsert(
        &db,
        "Robert Smith",
        EntityKind::Person,
        vec![],
        serde_json::Value::Object(Default::default()),
        None,
        &[],
    )
    .await
    .expect("upsert entity");

    // Memory whose summary contains the canonical name so FTS will match it.
    let id = db_memory::create(
        &db,
        db_memory::MemoryDraft::new(
            "Robert Smith reviewed the deployment plan".to_string(),
            MemoryType::Episodic,
            MemorySubtype::Meeting,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .expect("create memory");

    // Call match_known_entities, which runs FTS lookup and calls merge_anchors
    // with whatever canonical name the DB row contains.
    auto_link::match_known_entities(&db, &id, "Robert Smith reviewed the deployment plan")
        .await
        .expect("match_known_entities");

    // The stored anchor must be the canonical entity name, not a raw token variant.
    let mem = db_memory::get(&db, &id).await.expect("get memory");
    let anchors = mem.anchor_entities.unwrap_or_default();
    assert!(
        anchors.contains(&"Robert Smith".to_string()),
        "anchor_entities must contain canonical name 'Robert Smith'; got {anchors:?}"
    );
}

/// `merge_anchors` uses `array::union` semantics — it must not overwrite anchors
/// already written by another path (e.g. sidecar extract_anchors).
///
/// Simulate concurrent paths:
/// 1. `set_anchors` writes the sidecar-injected set {"Alice", "Project X"}.
/// 2. `merge_anchors` is called with {"Robert Smith"} (auto_link path).
/// 3. Assert the union {"Alice", "Project X", "Robert Smith"} is stored — no
///    element is lost, no element is duplicated.
#[tokio::test]
async fn merge_anchors_unions_with_existing_without_overwrite() {
    let (db, _dir) = test_db().await;

    let id = db_memory::create(
        &db,
        db_memory::MemoryDraft::new(
            "Alice reviewed Project X with Robert Smith".to_string(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .expect("create memory");

    // Step 1 — sidecar path: set initial anchors.
    let sidecar_anchors = vec!["Alice".to_string(), "Project X".to_string()];
    db_memory::set_anchors(&db, &id, &sidecar_anchors)
        .await
        .expect("set_anchors (sidecar simulation)");

    // Verify initial anchors are persisted before merging.
    let mem_before = db_memory::get(&db, &id).await.expect("get before merge");
    let before = mem_before.anchor_entities.unwrap_or_default();
    assert_eq!(before.len(), 2, "expected 2 sidecar anchors before merge; got {before:?}");

    // Step 2 — auto_link path: merge a new canonical name.
    let autolink_anchors = vec!["Robert Smith".to_string()];
    db_memory::merge_anchors(&db, &id, &autolink_anchors)
        .await
        .expect("merge_anchors");

    // Step 3 — union must contain all three; no duplicates, none overwritten.
    let mem_after = db_memory::get(&db, &id).await.expect("get after merge");
    let after = mem_after.anchor_entities.unwrap_or_default();

    assert_eq!(
        after.len(),
        3,
        "union of 2 sidecar + 1 autolink anchors must yield 3 elements; got {after:?}"
    );
    assert!(after.contains(&"Alice".to_string()), "sidecar anchor 'Alice' must be preserved; got {after:?}");
    assert!(after.contains(&"Project X".to_string()), "sidecar anchor 'Project X' must be preserved; got {after:?}");
    assert!(after.contains(&"Robert Smith".to_string()), "autolink anchor 'Robert Smith' must be present; got {after:?}");
}

/// Calling `merge_anchors` twice with overlapping sets must be idempotent —
/// duplicate entries must not accumulate.
#[tokio::test]
async fn merge_anchors_is_idempotent_for_duplicate_calls() {
    let (db, _dir) = test_db().await;

    let id = db_memory::create(
        &db,
        db_memory::MemoryDraft::new(
            "Alice resolved the deployment issue".to_string(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .expect("create memory");

    let anchors = vec!["Alice".to_string()];

    // Call merge_anchors twice with the same set.
    db_memory::merge_anchors(&db, &id, &anchors)
        .await
        .expect("first merge_anchors");
    db_memory::merge_anchors(&db, &id, &anchors)
        .await
        .expect("second merge_anchors");

    let mem = db_memory::get(&db, &id).await.expect("get");
    let stored = mem.anchor_entities.unwrap_or_default();
    assert_eq!(
        stored.len(),
        1,
        "duplicate merge_anchors calls must not accumulate entries; got {stored:?}"
    );
}

/// A Sensitive-visibility memory should not receive anchor_entities after write,
/// because the post-create anchor task is gated on `visibility != Sensitive`.
/// This test verifies the field is NULL immediately after create (without sidecar).
#[tokio::test]
async fn sensitive_memory_anchor_entities_null() {
    let (db, _dir) = test_db().await;

    let id = db_memory::create(
        &db,
        db_memory::MemoryDraft::new(
            "top-secret deployment keys stored in vault".to_string(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .visibility(Visibility::Sensitive),
    )
    .await
    .expect("create sensitive memory");

    let mem = db_memory::get(&db, &id).await.expect("get");
    assert!(
        mem.anchor_entities.is_none(),
        "sensitive memory must not have anchor_entities populated immediately after create"
    );
}
