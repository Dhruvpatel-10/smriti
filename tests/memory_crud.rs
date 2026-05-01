use chrono::{Duration, Utc};
use smriti::db::{client, embeddings::store_vector_pub, memory};
use smriti::types::*;

mod common;
use common::{test_db, create_test_memory};

// ═══════════════════════════════════════════════════════════════════════════
// MEMORY CRUD
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn create_and_get_memory() {
    let (db, _dir) = test_db().await;

    let id = memory::create(&db,
        memory::MemoryDraft::new("Decided to use SurrealDB for embedded storage".into(), MemoryType::Semantic, MemorySubtype::Decision, Some(Channel::Cli), Some(Modality::Text))
            .content("Full reasoning: graph relations, no daemon, SurrealKV engine".into())
            .project("knowledge-base".into())
            .people(vec!["alice".into()])
            .importance(Importance::new(5).unwrap())
            .tags(vec!["architecture".into(), "database".into()]))
    .await
    .expect("create failed");

    assert!(id.as_str().starts_with("mem_"));

    let mem = memory::get(&db, &id).await.expect("get failed");
    assert_eq!(mem.summary, "Decided to use SurrealDB for embedded storage");
    assert_eq!(mem.content.as_deref(), Some("Full reasoning: graph relations, no daemon, SurrealKV engine"));
    assert_eq!(mem.memory_type, MemoryType::Semantic);
    assert_eq!(mem.subtype, MemorySubtype::Decision);
    assert_eq!(mem.channel, Some(Channel::Cli));
    assert_eq!(mem.modality, Some(Modality::Text));
    assert_eq!(mem.project.as_deref(), Some("knowledge-base"));
    assert_eq!(mem.people, vec!["alice"]);
    assert_eq!(mem.importance.value(), 5);
    assert_eq!(mem.status, MemoryStatus::Active);
    assert_eq!(mem.tags, vec!["architecture", "database"]);
    assert_eq!(mem.access_count, 0);
    assert!(mem.expires_at.is_none());
}

#[tokio::test]
async fn create_rejects_invalid_type_subtype_pairing() {
    let (db, _dir) = test_db().await;

    let result = memory::create(&db,
        memory::MemoryDraft::new("bad pairing".into(), MemoryType::Episodic, MemorySubtype::Decision, Some(Channel::Cli), Some(Modality::Text)))
    .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("not valid for type"),
        "expected type/subtype validation error, got: {err}"
    );
}

#[tokio::test]
async fn get_nonexistent_memory_returns_not_found() {
    let (db, _dir) = test_db().await;

    let result = memory::get(&db, &MemoryId::from("nonexistent".to_string())).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn get_and_touch_increments_access_count() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "touchable memory").await;

    let mem = memory::get(&db, &id).await.unwrap();
    assert_eq!(mem.access_count, 0);

    let mem = memory::get_and_touch(&db, &id).await.unwrap();
    assert_eq!(mem.access_count, 1);

    let mem = memory::get_and_touch(&db, &id).await.unwrap();
    assert_eq!(mem.access_count, 2);
}

#[tokio::test]
async fn archive_memory() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "to be archived").await;
    memory::archive(&db, &id, InvalidationReason::Forgotten).await.expect("archive failed");

    let mem = memory::get(&db, &id).await.unwrap();
    assert_eq!(mem.status, MemoryStatus::Archived);
}

#[tokio::test]
async fn archive_nonexistent_returns_error() {
    let (db, _dir) = test_db().await;

    let result = memory::archive(&db, &MemoryId::from("fake_id".to_string()), InvalidationReason::Forgotten).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn supersede_memory() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "old decision").await;
    memory::supersede(&db, &id, Utc::now()).await.expect("supersede failed");

    let mem = memory::get(&db, &id).await.unwrap();
    // supersede now writes status='archived'; invalidation_reason is the source of truth
    assert_eq!(mem.status, MemoryStatus::Archived);
    assert_eq!(mem.invalidation_reason, Some(InvalidationReason::Superseded));
}

#[tokio::test]
async fn promote_clears_expiry_and_bumps_importance() {
    let (db, _dir) = test_db().await;

    let id = memory::create(&db,
        memory::MemoryDraft::new("scratch note".into(), MemoryType::Working, MemorySubtype::Scratch, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(1).unwrap()))
    .await
    .unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    assert!(mem.expires_at.is_some(), "scratch should have TTL");
    assert_eq!(mem.importance.value(), 1);

    memory::promote(&db, &id).await.expect("promote failed");

    let mem = memory::get(&db, &id).await.unwrap();
    assert!(mem.expires_at.is_none(), "promoted memory should have no TTL");
    assert!(mem.importance.value() >= 3, "importance should be bumped to at least 3");
}

#[tokio::test]
async fn promote_preserves_high_importance() {
    let (db, _dir) = test_db().await;

    let id = memory::create(&db,
        memory::MemoryDraft::new("important scratch".into(), MemoryType::Working, MemorySubtype::Priority, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(5).unwrap()))
    .await
    .unwrap();

    memory::promote(&db, &id).await.unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    assert_eq!(mem.importance.value(), 5, "should not reduce existing high importance");
}

#[tokio::test]
async fn working_scratch_gets_48h_ttl() {
    let (db, _dir) = test_db().await;

    let id = memory::create(&db,
        memory::MemoryDraft::new("scratch".into(), MemoryType::Working, MemorySubtype::Scratch, Some(Channel::Cli), Some(Modality::Text)))
    .await
    .unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    let expires = mem.expires_at.expect("scratch should auto-expire");
    let delta = expires - mem.created_at;
    assert!(
        (delta - Duration::hours(48)).num_seconds().unsigned_abs() < 5,
        "scratch TTL should be ~48h, got: {delta}"
    );
}

#[tokio::test]
async fn working_priority_gets_7d_ttl() {
    let (db, _dir) = test_db().await;

    let id = memory::create(&db,
        memory::MemoryDraft::new("priority".into(), MemoryType::Working, MemorySubtype::Priority, Some(Channel::Cli), Some(Modality::Text)))
    .await
    .unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    let expires = mem.expires_at.expect("priority should auto-expire");
    let delta = expires - mem.created_at;
    assert!(
        (delta - Duration::days(7)).num_seconds().unsigned_abs() < 5,
        "priority TTL should be ~7d, got: {delta}"
    );
}

#[tokio::test]
async fn working_open_question_gets_30d_ttl() {
    let (db, _dir) = test_db().await;

    let id = memory::create(&db,
        memory::MemoryDraft::new("open question".into(), MemoryType::Working, MemorySubtype::OpenQuestion, Some(Channel::Cli), Some(Modality::Text)))
    .await
    .unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    let expires = mem.expires_at.expect("open question should auto-expire");
    let delta = expires - mem.created_at;
    assert!(
        (delta - Duration::days(30)).num_seconds().unsigned_abs() < 5,
        "open question TTL should be ~30d, got: {delta}"
    );
}

#[tokio::test]
async fn semantic_decision_has_no_ttl() {
    let (db, _dir) = test_db().await;

    let id = memory::create(&db,
        memory::MemoryDraft::new("permanent decision".into(), MemoryType::Semantic, MemorySubtype::Decision, Some(Channel::Cli), Some(Modality::Text)))
    .await
    .unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    assert!(mem.expires_at.is_none(), "semantic decisions should not auto-expire");
}

#[tokio::test]
async fn count_memories() {
    let (db, _dir) = test_db().await;

    assert_eq!(memory::count(&db, None).await.unwrap(), 0);

    create_test_memory(&db, "one").await;
    create_test_memory(&db, "two").await;

    assert_eq!(memory::count(&db, None).await.unwrap(), 2);
    assert_eq!(memory::count(&db, Some(MemoryStatus::Active)).await.unwrap(), 2);
    assert_eq!(memory::count(&db, Some(MemoryStatus::Archived)).await.unwrap(), 0);
}

#[tokio::test]
async fn event_at_defaults_to_now() {
    let (db, _dir) = test_db().await;
    let before = Utc::now();

    let id = create_test_memory(&db, "auto event_at").await;

    let mem = memory::get(&db, &id).await.unwrap();
    let after = Utc::now();

    assert!(mem.event_at >= before - Duration::seconds(1));
    assert!(mem.event_at <= after + Duration::seconds(1));
}

#[tokio::test]
async fn explicit_event_at_is_preserved() {
    let (db, _dir) = test_db().await;
    let past = Utc::now() - Duration::days(30);

    let id = memory::create(&db,
        memory::MemoryDraft::new("past event".into(), MemoryType::Episodic, MemorySubtype::Meeting, None, Some(Modality::Transcript))
            .event_at(past))
    .await
    .unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    assert!(
        (mem.event_at - past).num_seconds().unsigned_abs() < 2,
        "event_at should be the explicit past time"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// SCHEMA IDEMPOTENCY
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn connect_twice_same_path_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();

    let db1 = client::connect(dir.path()).await.expect("first connect");
    create_test_memory(&db1, "before reconnect").await;
    drop(db1);
    // SurrealKV v3 releases the file lock on a background blocking thread.
    // Give it time to complete before we attempt to reopen the same path.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let db2 = client::connect(dir.path()).await.expect("second connect");
    let count = memory::count(&db2, None).await.unwrap();
    assert_eq!(count, 1, "data should persist across reconnects");
}

// ═══════════════════════════════════════════════════════════════════════════
// TOCTOU / ATOMIC DEDUP
// ═══════════════════════════════════════════════════════════════════════════

/// Verify that two sequential `create_with_dedup_atomic` calls with an identical
/// embedding vector result in exactly one accepted write and one duplicate rejection.
///
/// This tests the atomic transaction guarantee: the second call sees the first
/// record in its dedup scan (both vector storage and record creation are atomic
/// within the same transaction) and fires THROW.
///
/// Note: SurrealKV is single-writer, so we can't truly have two concurrent
/// transactions. Sequential calls with vector pre-seeded test the same property:
/// the dedup scan inside the transaction sees already-committed records.
#[tokio::test]
async fn dedup_atomic_second_identical_call_rejected() {
    let (db, _dir) = test_db().await;

    // A unit vector shared by both writers — cosine similarity = 1.0 > REJECT_THRESHOLD.
    let shared_vector: Vec<f32> = {
        let mut v = vec![0.0_f32; 256];
        v[0] = 1.0;
        v
    };

    // First writer creates a record and immediately stores its vector
    // (simulating what execute_inner does post-create).
    let draft1 = memory::MemoryDraft::new(
        "First writer — dedup atomicity test memory".to_string(),
        MemoryType::Semantic,
        MemorySubtype::Fact,
        Some(Channel::Cli),
        Some(Modality::Text),
    );
    let (first_id, _warn) = memory::create_with_dedup_atomic(&db, draft1, shared_vector.clone(), None)
        .await
        .expect("first create should succeed");

    // Store the vector so it is visible to the next call's dedup scan.
    store_vector_pub(&db, first_id.as_str(), shared_vector.clone())
        .await
        .expect("vector store for first record failed");

    // Second writer tries with the same vector — should be rejected as duplicate.
    let draft2 = memory::MemoryDraft::new(
        "Second writer — dedup atomicity test memory".to_string(),
        MemoryType::Semantic,
        MemorySubtype::Fact,
        Some(Channel::Cli),
        Some(Modality::Text),
    );
    let r2 = memory::create_with_dedup_atomic(&db, draft2, shared_vector.clone(), None).await;

    assert!(
        matches!(&r2, Err(smriti::error::KbError::InvalidInput(msg)) if msg.contains("duplicate")),
        "second create with identical vector should be rejected as duplicate; got: {r2:?}"
    );

    // Only one memory record should exist (the first one, not the rejected second).
    let total = memory::count(&db, None).await.unwrap();
    assert_eq!(total, 1, "only one memory should exist after dedup rejection");
}
