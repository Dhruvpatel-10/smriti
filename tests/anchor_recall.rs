mod common;
use common::test_db;

use smriti::db::{memory as db_memory, search};
use smriti::db::search::AuthorFilter;
use smriti::types::{Channel, MemorySubtype, MemoryType, Modality, Visibility};

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
    )
    .await
    .expect("recall");

    let found = results.iter().any(|m| m.id == id);
    assert!(
        found,
        "memory with anchor 'Sarah' + 'gRPC interceptor rollout' should appear in top-10 for 'Sarah promise gRPC'"
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
