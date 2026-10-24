mod common;
use common::{test_db, create_test_memory, MockEmbedder};

use chrono::{Duration, Utc};
use smriti::db::{chunking, memory, prune};
use smriti::config::PruningConfig;
use smriti::types::*;

use std::sync::Arc;
use smriti::db::embeddings::Embedder;

// ═══════════════════════════════════════════════════════════════════════════
// PRUNING
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn ttl_expiry_archives_expired_memories() {
    let (db, _dir) = test_db().await;

    memory::create(&db,
        memory::MemoryDraft::new("expired scratch".into(), MemoryType::Working, MemorySubtype::Scratch, Some(Channel::Cli), Some(Modality::Text))
            .expires_at(Utc::now() - Duration::hours(1))).await.unwrap();

    let future_id = memory::create(&db,
        memory::MemoryDraft::new("future scratch".into(), MemoryType::Working, MemorySubtype::Scratch, Some(Channel::Cli), Some(Modality::Text))
            .expires_at(Utc::now() + Duration::days(30))).await.unwrap();

    let expired = prune::expire_ttl(&db).await.unwrap();
    assert_eq!(expired, 1, "should expire exactly 1 memory");

    let mem = memory::get(&db, &future_id).await.unwrap();
    assert_eq!(mem.status, MemoryStatus::Active);
}

/// Disuse never lowers importance or archives a memory (protocol I5: salience is
/// a prior, never a gate). Only an explicit `expires_at`/TTL archives.
#[tokio::test]
async fn prune_never_decays_or_archives_unused_memories() {
    let (db, _dir) = test_db().await;

    let config = PruningConfig {
        working_scratch_ttl_hours: 48,
        working_priority_ttl_days: 7,
        working_open_question_ttl_days: 30,
        access_decay_interval_days: 0,
        consolidation_interval_hours: 24,
        importance_archive_threshold: 5,
    };

    let id = memory::create(&db,
        memory::MemoryDraft::new("unaccessed memory".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(2).unwrap())).await.unwrap();

    db.query("UPDATE type::record('memory', $id) SET updated_at = time::now() - 400d, next_review_at = time::now() - 300d")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();

    prune::run_all(&db, &config).await.unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    assert_eq!(mem.importance.value(), 2, "importance is the caller's prior and never decays");
    assert_eq!(mem.status, MemoryStatus::Active, "disuse must not archive");
}

#[tokio::test]
async fn prune_run_all_does_not_crash_on_empty_db() {
    let (db, _dir) = test_db().await;

    let config = PruningConfig {
        working_scratch_ttl_hours: 48,
        working_priority_ttl_days: 7,
        working_open_question_ttl_days: 30,
        access_decay_interval_days: 7,
        consolidation_interval_hours: 24,
        importance_archive_threshold: 1,
    };

    prune::run_all(&db, &config).await.expect("prune should not crash on empty db");
}

#[tokio::test]
async fn ttl_expiry_does_not_touch_no_expiry_memories() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "permanent fact").await;

    let expired = prune::expire_ttl(&db).await.unwrap();
    assert_eq!(expired, 0);

    let mem = memory::get(&db, &id).await.unwrap();
    assert_eq!(mem.status, MemoryStatus::Active);
}

// ═══════════════════════════════════════════════════════════════════════════
// TTL INVARIANT TESTS — chunk inheritance and recall filtering
// ═══════════════════════════════════════════════════════════════════════════

/// Build large content (≥CHUNK_THRESHOLD) for chunking tests.
fn large_content() -> String {
    let mut s = String::new();
    for i in 0..20 {
        s.push_str(&format!(
            "Paragraph {i}: Detailed content about architecture decisions, SurrealDB \
             storage, embedding pipelines, and chunking strategies for large documents.\n\n"
        ));
    }
    assert!(s.len() >= chunking::CHUNK_THRESHOLD);
    s
}

/// Each chunk created by `chunk_and_embed` must inherit the parent's `expires_at`
/// exactly. TTL propagation from parent to child is a correctness invariant — if
/// chunks outlive parents, stale content leaks into search results.
#[tokio::test]
async fn chunk_inherits_parent_expires_at() {
    let (db, _dir) = test_db().await;

    // Future expiry — well within range, so the parent stays active.
    let parent_expires = Utc::now() + Duration::hours(1);
    let content = large_content();

    let parent = chunking::ChunkParent {
        summary: "TTL inheritance test parent".to_string(),
        content: content.clone(),
        memory_type: MemoryType::Semantic,
        subtype: MemorySubtype::Fact,
        channel: Some(Channel::Cli),
        modality: Some(Modality::Text),
        project: None,
        domain: vec![],
        importance: Importance::new(3).unwrap(),
        visibility: Visibility::Standard,
        event_at: Utc::now(),
        tags: vec![],
        expires_at: Some(parent_expires),
        source_agent: None,
        session_id: None,
        author: None,
    };

    let parent_id = memory::create(
        &db,
        memory::MemoryDraft::new(
            parent.summary.clone(),
            parent.memory_type,
            parent.subtype,
            parent.channel,
            parent.modality,
        )
        .content(content)
        .importance(parent.importance)
        .visibility(parent.visibility)
        .event_at(parent.event_at)
        .maybe_expires_at(parent.expires_at),
    )
    .await
    .expect("create parent");

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default_1024());
    let chunk_ids = chunking::chunk_and_embed(&db, embedder, &parent_id, &parent)
        .await
        .expect("chunk_and_embed");

    assert!(
        chunk_ids.len() >= 2,
        "need at least 2 chunks for this test; got {}",
        chunk_ids.len()
    );

    for chunk_id in &chunk_ids {
        let chunk = memory::get(&db, chunk_id)
            .await
            .unwrap_or_else(|_| panic!("chunk {chunk_id} must exist"));

        let chunk_expires = chunk
            .expires_at
            .unwrap_or_else(|| panic!("chunk {chunk_id} must have expires_at set"));

        // Allow 1-second tolerance for the timestamp comparison to absorb
        // any sub-second timing skew between parent creation and chunk creation.
        let delta = (chunk_expires - parent_expires).num_milliseconds().abs();
        assert!(
            delta < 1000,
            "chunk {chunk_id} expires_at must match parent expires_at within 1s; \
             parent={parent_expires}, chunk={chunk_expires}, delta={delta}ms"
        );
    }
}

/// Chunks whose parent has already expired must be invisible to `kb_recall`.
///
/// `BITEMPORAL_ACTIVE` predicate filters any record whose `expires_at` is in
/// the past. By inserting a parent with `expires_at = now() - 1s` we simulate
/// an already-expired memory without needing a clock injection.
#[tokio::test]
async fn chunks_vanish_from_recall_when_parent_expires() {
    let (db, _dir) = test_db().await;

    // Past expiry — already expired at the moment of insertion.
    let past_expires = Utc::now() - Duration::seconds(2);
    let content = large_content();

    let parent = chunking::ChunkParent {
        summary: "TTL expired parent for chunk recall test".to_string(),
        content: content.clone(),
        memory_type: MemoryType::Semantic,
        subtype: MemorySubtype::Fact,
        channel: Some(Channel::Cli),
        modality: Some(Modality::Text),
        project: None,
        domain: vec![],
        importance: Importance::new(3).unwrap(),
        visibility: Visibility::Standard,
        event_at: Utc::now(),
        tags: vec![],
        expires_at: Some(past_expires),
        source_agent: None,
        session_id: None,
        author: None,
    };

    let parent_id = memory::create(
        &db,
        memory::MemoryDraft::new(
            parent.summary.clone(),
            parent.memory_type,
            parent.subtype,
            parent.channel,
            parent.modality,
        )
        .content(content)
        .importance(parent.importance)
        .visibility(parent.visibility)
        .event_at(parent.event_at)
        .maybe_expires_at(parent.expires_at),
    )
    .await
    .expect("create parent");

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default_1024());
    let chunk_ids = chunking::chunk_and_embed(&db, embedder, &parent_id, &parent)
        .await
        .expect("chunk_and_embed");

    assert!(
        chunk_ids.len() >= 2,
        "need at least 2 chunks for this test; got {}",
        chunk_ids.len()
    );

    // The parent itself must be filtered from BITEMPORAL_ACTIVE queries.
    let mut result = db
        .query(
            "SELECT * FROM memory WHERE invalidated_at IS NONE \
             AND (valid_until IS NONE OR valid_until > time::now()) \
             AND (expires_at IS NONE OR expires_at > time::now())",
        )
        .await
        .expect("bitemporal query");
    let active: Vec<serde_json::Value> = result.take(0).unwrap_or_default();

    // All rows (parent + chunks) were inserted with past expires_at.
    // The BITEMPORAL_ACTIVE predicate must filter ALL of them.
    assert!(
        active.is_empty(),
        "parent and all chunks must be filtered by BITEMPORAL_ACTIVE when expires_at is past; \
         found {} active row(s)",
        active.len()
    );
}
