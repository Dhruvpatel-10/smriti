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

#[tokio::test]
async fn access_decay_reduces_importance() {
    let (db, _dir) = test_db().await;

    let config = PruningConfig {
        working_scratch_ttl_hours: 48,
        working_priority_ttl_days: 7,
        working_open_question_ttl_days: 30,
        access_decay_interval_days: 0,
        consolidation_interval_hours: 24,
        importance_archive_threshold: 1,
    };

    let id = memory::create(&db,
        memory::MemoryDraft::new("unaccessed memory".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(2).unwrap())).await.unwrap();

    db.query("UPDATE type::record('memory', $id) SET updated_at = time::now() - 2d")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();

    prune::run_all(&db, &config).await.unwrap();

    // Decay fires for importance > threshold (2 > 1), dropping to 1.
    // archive_below_threshold fires for importance < threshold (1 < 1 is false),
    // so the memory stays Active at the floor importance.
    let mem = memory::get(&db, &id).await.unwrap();
    assert_eq!(mem.importance.value(), 1, "importance should drop by 1 to the floor");
    assert_eq!(mem.status, MemoryStatus::Active, "memory at threshold floor must not be archived");
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
// SM-2 EXPONENTIAL DECAY TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn sm2_decay_skips_recently_reviewed_memories() {
    let (db, _dir) = test_db().await;

    let config = PruningConfig {
        working_scratch_ttl_hours: 48,
        working_priority_ttl_days: 7,
        working_open_question_ttl_days: 30,
        access_decay_interval_days: 7,
        consolidation_interval_hours: 24,
        importance_archive_threshold: 1,
    };

    let id = memory::create(&db,
        memory::MemoryDraft::new("recently reviewed".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(3).unwrap())).await.unwrap();

    // Set next_review_at to FUTURE — this memory is NOT overdue
    db.query("UPDATE type::record('memory', $id) SET next_review_at = time::now() + 7d, ease_factor = 2.5, review_interval = 7")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();

    prune::run_all(&db, &config).await.unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    assert_eq!(mem.importance.value(), 3, "importance should NOT decay when review is not overdue");
    assert_eq!(mem.status, MemoryStatus::Active);
}

#[tokio::test]
async fn sm2_decay_reduces_overdue_unaccessed_memories() {
    let (db, _dir) = test_db().await;

    let config = PruningConfig {
        working_scratch_ttl_hours: 48,
        working_priority_ttl_days: 7,
        working_open_question_ttl_days: 30,
        access_decay_interval_days: 7,
        consolidation_interval_hours: 24,
        importance_archive_threshold: 1,
    };

    let id = memory::create(&db,
        memory::MemoryDraft::new("overdue memory".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(4).unwrap())).await.unwrap();

    // Set next_review_at to PAST and access_count to 0 — overdue, unaccessed
    db.query("UPDATE type::record('memory', $id) SET next_review_at = time::now() - 3d, ease_factor = 2.5, review_interval = 7, access_count = 0")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();

    prune::sm2_decay(&db, &config).await.unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    assert_eq!(mem.importance.value(), 3, "importance should drop by 1 for overdue memory");
}

#[tokio::test]
async fn sm2_decay_resets_review_interval_on_miss() {
    let (db, _dir) = test_db().await;

    let config = PruningConfig {
        working_scratch_ttl_hours: 48,
        working_priority_ttl_days: 7,
        working_open_question_ttl_days: 30,
        access_decay_interval_days: 7,
        consolidation_interval_hours: 24,
        importance_archive_threshold: 1,
    };

    // Use importance=4 (below immunity threshold of 5) so decay applies
    let id = memory::create(&db,
        memory::MemoryDraft::new("forgotten memory".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(4).unwrap())).await.unwrap();

    // Set high review_interval (was being recalled regularly) but now overdue
    db.query("UPDATE type::record('memory', $id) SET next_review_at = time::now() - 1d, ease_factor = 3.5, review_interval = 30, access_count = 0")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();

    prune::sm2_decay(&db, &config).await.unwrap();

    // After a miss: review_interval resets to 1, ease_factor decreases
    let mut result = db.query("SELECT review_interval, ease_factor FROM type::record('memory', $id)")
        .bind(("id", id.as_str().to_string()))
        .await.unwrap();
    let row: Option<serde_json::Value> = result.take(0).unwrap();
    let row = row.unwrap();

    let ri = row.get("review_interval").and_then(|v| v.as_i64()).unwrap();
    let ef = row.get("ease_factor").and_then(|v| v.as_f64()).unwrap();

    assert_eq!(ri, 1, "review_interval should reset to 1 after a missed review");
    assert!((ef - 3.3).abs() < 0.01, "ease_factor should decrease by 0.2 (from 3.5 to 3.3), got {ef}");
}

#[tokio::test]
async fn sm2_decay_ease_factor_floor_at_1_3() {
    let (db, _dir) = test_db().await;

    let config = PruningConfig {
        working_scratch_ttl_hours: 48,
        working_priority_ttl_days: 7,
        working_open_question_ttl_days: 30,
        access_decay_interval_days: 7,
        consolidation_interval_hours: 24,
        importance_archive_threshold: 1,
    };

    // Use importance=4 (below immunity threshold of 5) so decay applies
    let id = memory::create(&db,
        memory::MemoryDraft::new("hard to remember".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(4).unwrap())).await.unwrap();

    // Set ease_factor near minimum (1.4), simulate overdue
    db.query("UPDATE type::record('memory', $id) SET next_review_at = time::now() - 1d, ease_factor = 1.4, review_interval = 1, access_count = 0")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();

    prune::sm2_decay(&db, &config).await.unwrap();

    let mut result = db.query("SELECT ease_factor FROM type::record('memory', $id)")
        .bind(("id", id.as_str().to_string()))
        .await.unwrap();
    let row: Option<serde_json::Value> = result.take(0).unwrap();
    let ef = row.unwrap().get("ease_factor").and_then(|v| v.as_f64()).unwrap();

    assert!((ef - 1.3).abs() < 0.01, "ease_factor should floor at 1.3, got {ef}");
}

#[tokio::test]
async fn legacy_decay_still_works_without_sm2_fields() {
    let (db, _dir) = test_db().await;

    let config = PruningConfig {
        working_scratch_ttl_hours: 48,
        working_priority_ttl_days: 7,
        working_open_question_ttl_days: 30,
        access_decay_interval_days: 0, // immediate decay for testing
        consolidation_interval_hours: 24,
        importance_archive_threshold: 1,
    };

    let id = memory::create(&db,
        memory::MemoryDraft::new("pre-migration memory".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(3).unwrap())).await.unwrap();

    // Ensure NO sm2 fields, old updated_at
    db.query("UPDATE type::record('memory', $id) SET next_review_at = NONE, ease_factor = NONE, updated_at = time::now() - 2d")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();

    prune::sm2_decay(&db, &config).await.unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    assert_eq!(mem.importance.value(), 2, "legacy decay should reduce importance by 1");
}

/// Regression test for the off-by-one boundary between sm2_decay and
/// archive_below_threshold.
///
/// Original bug: `archive_below_threshold` used `importance <= threshold`, which
/// immediately archived memories AT the threshold without ever giving them a decay
/// step. This is silent data loss — the memory vanishes the first time it touches
/// the boundary, bypassing the SM-2 decay path entirely.
///
/// Correct behaviour (after fix):
/// - `sm2_decay` fires for `importance > threshold` — memories decay down to the
///   threshold and stop there (it is the importance floor for SM-2).
/// - `archive_below_threshold` fires for `importance < threshold` (strict) — only
///   memories already below the floor are archived (e.g. manually adjusted records).
/// - A memory at exactly `importance == threshold` is neither decayed further nor
///   immediately archived; it stays Active at the floor.
#[tokio::test]
async fn decay_at_threshold_boundary_not_immediately_archived() {
    let (db, _dir) = test_db().await;

    // Use threshold=2 so we can create a memory at importance=3, watch it decay
    // to threshold=2, and verify it is not then immediately archived.
    let threshold = 2u8;
    let config = PruningConfig {
        working_scratch_ttl_hours: 48,
        working_priority_ttl_days: 7,
        working_open_question_ttl_days: 30,
        access_decay_interval_days: 0,
        consolidation_interval_hours: 24,
        importance_archive_threshold: threshold,
    };

    // Memory starts one step above threshold so it decays to threshold in cycle 1.
    let id = memory::create(&db,
        memory::MemoryDraft::new("boundary importance memory".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(threshold + 1).unwrap())).await.unwrap();

    // Set up SM-2 fields as overdue so the SM-2 decay path fires.
    db.query("UPDATE type::record('memory', $id) SET next_review_at = time::now() - 3d, ease_factor = 2.5, review_interval = 7, access_count = 0")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();

    // Cycle 1: importance=threshold+1 > threshold, decay fires → drops to threshold.
    // archive_below_threshold: threshold < threshold is false → NOT archived.
    prune::run_all(&db, &config).await.unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    assert_eq!(
        mem.importance.value(), threshold,
        "importance must drop by 1 to the floor threshold"
    );
    assert_eq!(
        mem.status, MemoryStatus::Active,
        "memory that decayed to threshold must not be immediately archived (off-by-one regression)"
    );

    // Cycle 2: importance=threshold, threshold > threshold is false → NO decay.
    // archive_below_threshold: threshold < threshold is false → still Active.
    // SM-2 floors at the threshold; the memory stays alive until recalled.
    db.query("UPDATE type::record('memory', $id) SET next_review_at = time::now() - 3d, access_count = 0")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();

    prune::run_all(&db, &config).await.unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    assert_eq!(
        mem.importance.value(), threshold,
        "importance must not decay below the threshold floor"
    );
    assert_eq!(
        mem.status, MemoryStatus::Active,
        "memory at the threshold floor must remain Active (SM-2 decay stops at floor)"
    );
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
