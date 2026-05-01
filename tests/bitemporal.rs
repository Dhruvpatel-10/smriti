// ADR-011: Bitemporal memory Phase 1 integration tests.
//
// Covers:
//   - Field defaults & serde (ADR-011 §Schema)
//   - kb_retract MCP tool (ADR-011 §New Tool)
//   - Recall predicate excludes invalidated memories (ADR-011 §Retrieval Changes)
//   - recall_at bitemporal correctness fix (ADR-011 §kb_recall_at)
//   - Write-path reason propagation (forget, TTL expiry, consolidation)
//   - Consolidation valid_until semantics (ADR-011 §Consolidation Provenance)

mod common;
use common::{test_db, create_test_memory};

use chrono::{Duration, Utc};
use smriti::db::{memory, prune, search};
use smriti::db::search::AuthorFilter;
use smriti::tools::{archive, consolidate};
use smriti::types::*;

// ═══════════════════════════════════════════════════════════════════════════
// Field defaults & serde
// ═══════════════════════════════════════════════════════════════════════════

/// Newly created memory must have all three bitemporal fields absent (None).
#[tokio::test]
async fn create_defaults_bitemporal_fields_to_none() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "basic memory no bitemporal fields").await;
    let mem = memory::get(&db, &id).await.unwrap();

    assert!(mem.valid_until.is_none(), "valid_until should default to None");
    assert!(mem.invalidated_at.is_none(), "invalidated_at should default to None");
    assert!(mem.invalidation_reason.is_none(), "invalidation_reason should default to None");
}

/// UPDATE each of the 5 InvalidationReason variants via raw DB query and confirm
/// the round-trip through parse_memory_row preserves the enum value.
#[tokio::test]
async fn memory_invalidation_reason_roundtrip_all_variants() {
    let all_reasons = [
        InvalidationReason::Superseded,
        InvalidationReason::Retracted,
        InvalidationReason::Expired,
        InvalidationReason::Forgotten,
        InvalidationReason::Consolidated,
    ];

    for reason in all_reasons {
        let (db, _dir) = test_db().await;
        let id = create_test_memory(&db, "roundtrip target memory").await;

        // Write the reason string directly as SurrealDB would store it.
        db.query(
            "UPDATE type::record('memory', $id) SET invalidation_reason = $reason",
        )
        .bind(("id", id.as_str().to_string()))
        .bind(("reason", reason.as_str().to_string()))
        .await
        .unwrap();

        let mem = memory::get(&db, &id).await.unwrap();

        assert_eq!(
            mem.invalidation_reason,
            Some(reason),
            "round-trip failed for reason variant {:?}",
            reason
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Retract tool
// ═══════════════════════════════════════════════════════════════════════════

/// Retracting a memory sets invalidated_at, invalidation_reason=Retracted,
/// and status=Archived.
#[tokio::test]
async fn retract_sets_invalidated_and_reason() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "memory to be retracted as wrong").await;

    archive::execute(
        &db,
        archive::ArchiveInput {
            id: id.as_str().to_string(),
            reason: smriti::types::ArchiveReason::Retract,
            correction_id: None,
        },
    )
    .await
    .unwrap();

    // memory::get filters active only; use raw query to inspect archived record.
    let mut result = db
        .query("SELECT * FROM type::record('memory', $id)")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();
    let row: Option<serde_json::Value> = result.take(0).unwrap();
    let row = row.expect("record must still exist after retraction");

    let invalidated_at = row.get("invalidated_at");
    assert!(
        invalidated_at.is_some() && !invalidated_at.unwrap().is_null(),
        "invalidated_at must be set after retraction"
    );

    let reason = row
        .get("invalidation_reason")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert_eq!(reason, "retracted", "invalidation_reason must be 'retracted'");

    let status = row
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert_eq!(status, "archived", "status must be 'archived' after retraction");
}

/// When correction_id is provided, a `retracts` edge is created from the
/// correction memory to the retracted memory.
#[tokio::test]
async fn retract_with_correction_creates_edge() {
    let (db, _dir) = test_db().await;

    let wrong = create_test_memory(&db, "wrong memory to be retracted later").await;
    let correction = create_test_memory(&db, "correction memory replacing wrong one").await;

    archive::execute(
        &db,
        archive::ArchiveInput {
            id: wrong.as_str().to_string(),
            reason: smriti::types::ArchiveReason::Retract,
            correction_id: Some(correction.as_str().to_string()),
        },
    )
    .await
    .unwrap();

    // Verify the retracts edge: correction -> retracts -> wrong
    let mut result = db
        .query(
            "SELECT record::id(out) AS retracted_id FROM retracts WHERE in = type::record('memory', $id)",
        )
        .bind(("id", correction.as_str().to_string()))
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = result.take(0).unwrap();

    let retracted_ids: Vec<String> = rows
        .iter()
        .filter_map(|r| r.get("retracted_id").and_then(|v| v.as_str()).map(String::from))
        .collect();

    assert!(
        retracted_ids.contains(&wrong.as_str().to_string()),
        "retracts edge must point from correction to wrong memory; edges found: {:?}",
        retracted_ids
    );

    // The wrong memory must have invalidated_at set.
    let mut result2 = db
        .query("SELECT invalidated_at FROM type::record('memory', $id)")
        .bind(("id", wrong.as_str().to_string()))
        .await
        .unwrap();
    let row: Option<serde_json::Value> = result2.take(0).unwrap();
    let inv = row
        .as_ref()
        .and_then(|r| r.get("invalidated_at"));
    assert!(
        inv.is_some() && !inv.unwrap().is_null(),
        "wrong memory must have invalidated_at set"
    );
}

/// Retracting a non-existent ID returns a NotFound error.
#[tokio::test]
async fn retract_nonexistent_id_returns_not_found() {
    let (db, _dir) = test_db().await;

    let result = archive::execute(
        &db,
        archive::ArchiveInput {
            id: "mem_doesnotexist_at_all".to_string(),
            reason: smriti::types::ArchiveReason::Retract,
            correction_id: None,
        },
    )
    .await;

    assert!(result.is_err(), "retract of nonexistent ID must fail");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("not found") || err_msg.contains("NotFound"),
        "error should be NotFound, got: {err_msg}"
    );
}

/// Retracting a memory that is already invalidated returns InvalidInput
/// (idempotency guard — preserves first invalidation reason).
#[tokio::test]
async fn retract_already_invalidated_returns_invalid_input() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "memory retracted once already").await;

    // First retraction succeeds.
    archive::execute(
        &db,
        archive::ArchiveInput {
            id: id.as_str().to_string(),
            reason: smriti::types::ArchiveReason::Retract,
            correction_id: None,
        },
    )
    .await
    .unwrap();

    // Second retraction must fail.
    let result = archive::execute(
        &db,
        archive::ArchiveInput {
            id: id.as_str().to_string(),
            reason: smriti::types::ArchiveReason::Retract,
            correction_id: None,
        },
    )
    .await;

    assert!(result.is_err(), "second retraction must fail");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("already invalidated") || err_msg.contains("invalid input"),
        "error should be InvalidInput, got: {err_msg}"
    );
}

/// Double-retract with correction_id must not create a spurious second
/// `retracts` edge. Pre-flight check rejects already-invalidated targets
/// before the transaction's RELATE can fire.
#[tokio::test]
async fn retract_already_invalidated_does_not_create_spurious_edge() {
    let (db, _dir) = test_db().await;

    let wrong = create_test_memory(&db, "wrong memory retracted twice").await;
    let corr1 = create_test_memory(&db, "first correction memory body").await;
    let corr2 = create_test_memory(&db, "second correction attempt body").await;

    // First retraction with corr1 succeeds and creates one edge.
    archive::execute(
        &db,
        archive::ArchiveInput {
            id: wrong.as_str().to_string(),
            reason: smriti::types::ArchiveReason::Retract,
            correction_id: Some(corr1.as_str().to_string()),
        },
    )
    .await
    .unwrap();

    // Second retraction with corr2 must fail and NOT create an edge.
    let result = archive::execute(
        &db,
        archive::ArchiveInput {
            id: wrong.as_str().to_string(),
            reason: smriti::types::ArchiveReason::Retract,
            correction_id: Some(corr2.as_str().to_string()),
        },
    )
    .await;
    assert!(result.is_err(), "second retraction must fail");

    // Count retracts edges pointing at wrong — must be exactly 1 (corr1).
    let mut rows = db
        .query(
            "SELECT record::id(in) AS in_id FROM retracts WHERE out = type::record('memory', $id)",
        )
        .bind(("id", wrong.as_str().to_string()))
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = rows.take(0).unwrap();
    let in_ids: Vec<String> = rows
        .iter()
        .filter_map(|r| r.get("in_id").and_then(|v| v.as_str()).map(String::from))
        .collect();

    assert_eq!(
        in_ids.len(),
        1,
        "exactly one retracts edge expected, got: {in_ids:?}"
    );
    assert_eq!(
        in_ids[0],
        corr1.as_str(),
        "edge must come from corr1 (the successful retraction), got: {in_ids:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Recall excludes invalidated memories
// ═══════════════════════════════════════════════════════════════════════════

/// Retracted memories must not appear in recall results.
#[tokio::test]
async fn recall_excludes_retracted_memories() {
    let (db, _dir) = test_db().await;

    // Signature: db, summary, content, type, subtype, source, project, domain,
    //            people, importance, visibility, event_at, tags, expires_at,
    //            source_agent, session_id, author, valid_until
    let m1 = memory::create(&db,
        memory::MemoryDraft::new("the capital of France is Berlin (wrong)".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text)))
    .await
    .unwrap();

    let m2 = memory::create(&db,
        memory::MemoryDraft::new("the capital of France is Paris (correct)".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text)))
    .await
    .unwrap();

    // Retract the wrong memory.
    memory::retract(&db, &m1, None).await.unwrap();

    let results = search::recall(
        &db,
        "capital of France",
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
        &AuthorFilter::None, 0.0, &Default::default(), &Default::default(), RerankerPolicy::default(), None
    )
    .await
    .unwrap();

    let ids: Vec<&str> = results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        !ids.contains(&m1.as_str()),
        "retracted memory must not appear in recall"
    );
    assert!(
        ids.contains(&m2.as_str()),
        "non-retracted memory must still appear in recall"
    );
}

/// A working memory with TTL expired (expires_at in the past) must not appear
/// in recall after prune runs.
#[tokio::test]
async fn recall_excludes_expired_working_memory() {
    let (db, _dir) = test_db().await;

    let id = memory::create(&db,
        memory::MemoryDraft::new("scratch note about expired topic".into(), MemoryType::Working, MemorySubtype::Scratch, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(1).unwrap()))
    .await
    .unwrap();

    // Deterministically set expires_at to the past — no sleep needed.
    db.query("UPDATE type::record('memory', $id) SET expires_at = time::now() - 2h")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();

    // Run TTL prune.
    prune::expire_ttl(&db).await.unwrap();

    let results = search::recall(
        &db,
        "scratch note about expired topic",
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
        &AuthorFilter::None, 0.0, &Default::default(), &Default::default(), RerankerPolicy::default(), None
    )
    .await
    .unwrap();

    let ids: Vec<&str> = results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        !ids.contains(&id.as_str()),
        "TTL-expired memory must not appear in recall after prune"
    );
}

/// BITEMPORAL_ACTIVE predicate must exclude memories whose expires_at is in
/// the past even without running prune — read-time TTL filtering (probe-ttl-expiry-0002).
#[tokio::test]
async fn recall_excludes_past_expires_at_without_prune() {
    let (db, _dir) = test_db().await;

    let id = memory::create(&db,
        memory::MemoryDraft::new("scratch note read-time expiry check".into(), MemoryType::Working, MemorySubtype::Scratch, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(1).unwrap()))
    .await
    .unwrap();

    // Backdate expires_at to the past without going through prune.
    db.query("UPDATE type::record('memory', $id) SET expires_at = time::now() - 2h")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();

    // No prune — BITEMPORAL_ACTIVE must filter at read time.
    let results = search::recall(
        &db,
        "scratch note read-time expiry check",
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
        &AuthorFilter::None, 0.0, &Default::default(), &Default::default(), RerankerPolicy::default(), None
    )
    .await
    .unwrap();

    let ids: Vec<&str> = results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        !ids.contains(&id.as_str()),
        "BITEMPORAL_ACTIVE must exclude memory whose expires_at is in the past (no prune needed)"
    );
}

/// recall_at with as_of BEFORE expires_at must include the memory;
/// recall_at with as_of AFTER expires_at must exclude it.
#[tokio::test]
async fn recall_at_respects_expires_at_boundary() {
    let (db, _dir) = test_db().await;

    let id = memory::create(&db,
        memory::MemoryDraft::new("scratch note for point-in-time expiry boundary".into(), MemoryType::Working, MemorySubtype::Scratch, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(1).unwrap()))
    .await
    .unwrap();

    // Set expires_at to 1h in the future so the memory is currently live.
    // as_of = 2h in the future (after expiry) — must be absent.
    // as_of = time::now() + 30min (before expiry) — must be present.
    let expiry = Utc::now() + Duration::hours(1);
    db.query("UPDATE type::record('memory', $id) SET expires_at = $expiry")
        .bind(("id", id.as_str().to_string()))
        .bind(("expiry", expiry))
        .await
        .unwrap();

    // as_of = now + 30min (before expiry) — memory should appear.
    let as_of_before = Utc::now() + Duration::minutes(30);
    let before_results = search::recall_at(
        &db,
        "scratch note for point-in-time expiry boundary",
        None,
        None,
        as_of_before,
        None,
        None,
        None,
        None,
        10,
        &AuthorFilter::None,
        &Default::default(),
        true,
    )
    .await
    .unwrap();
    let before_ids: Vec<&str> = before_results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        before_ids.contains(&id.as_str()),
        "recall_at before expires_at must include the memory"
    );

    // as_of = now + 2h (after expiry) — memory should be absent.
    let as_of_after = Utc::now() + Duration::hours(2);
    let after_results = search::recall_at(
        &db,
        "scratch note for point-in-time expiry boundary",
        None,
        None,
        as_of_after,
        None,
        None,
        None,
        None,
        10,
        &AuthorFilter::None,
        &Default::default(),
        true,
    )
    .await
    .unwrap();
    let after_ids: Vec<&str> = after_results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        !after_ids.contains(&id.as_str()),
        "recall_at after expires_at must exclude the memory"
    );
}

/// A memory with valid_until in the past must be excluded from recall.
#[tokio::test]
async fn recall_excludes_memory_past_valid_until() {
    let (db, _dir) = test_db().await;

    let past_valid_until = Utc::now() - Duration::days(365);
    let id = memory::create(&db,
        memory::MemoryDraft::new("outdated fact with expired validity window".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .valid_until(past_valid_until))
    .await
    .unwrap();

    let results = search::recall(
        &db,
        "outdated fact with expired validity window",
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
        &AuthorFilter::None, 0.0, &Default::default(), &Default::default(), RerankerPolicy::default(), None
    )
    .await
    .unwrap();

    let ids: Vec<&str> = results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        !ids.contains(&id.as_str()),
        "memory with valid_until in the past must not appear in recall"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// recall_at bitemporal correctness
// ═══════════════════════════════════════════════════════════════════════════

/// recall_at with as_of between t1 and t2 must return the M1 (older superseded
/// fact) but not M2 (the superseding fact that didn't exist yet at that time).
#[tokio::test]
async fn recall_at_returns_superseded_fact_before_supersession() {
    let (db, _dir) = test_db().await;

    let t1 = Utc::now() - Duration::days(30);
    let t2 = Utc::now() - Duration::days(10);
    let t_middle = Utc::now() - Duration::days(20);

    // M1 was created at t1 with event_at=t1.
    let m1 = memory::create(&db,
        memory::MemoryDraft::new("Paris population was ten million at that time".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .event_at(t1))
    .await
    .unwrap();

    // Backdate created_at to t1 so recall_at sees it as existing then.
    db.query("UPDATE type::record('memory', $id) SET created_at = $t1, event_at = $t1")
        .bind(("id", m1.as_str().to_string()))
        .bind(("t1", t1))
        .await
        .unwrap();

    // Supersede M1 at t2: sets valid_until=t2, invalidated_at=now, reason=Superseded.
    memory::supersede(&db, &m1, t2).await.unwrap();

    // M2 is the superseder, created at t2.
    let m2 = memory::create(&db,
        memory::MemoryDraft::new("Paris population was eleven million updated figure".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .event_at(t2))
    .await
    .unwrap();

    // Backdate M2's created_at to t2.
    db.query("UPDATE type::record('memory', $id) SET created_at = $t2, event_at = $t2")
        .bind(("id", m2.as_str().to_string()))
        .bind(("t2", t2))
        .await
        .unwrap();

    // recall_at(as_of=t_middle): M1 was valid, M2 didn't exist yet.
    let results = search::recall_at(
        &db,
        "Paris population",
        None,
        None,
        t_middle,
        None,
        None,
        None,
        None,
        10,
        &AuthorFilter::None,
        &Default::default(),
        true,
    )
    .await
    .unwrap();

    let ids: Vec<&str> = results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        ids.contains(&m1.as_str()),
        "M1 should be visible at t_middle (before supersession at t2)"
    );
    assert!(
        !ids.contains(&m2.as_str()),
        "M2 should NOT be visible at t_middle (created at t2, after t_middle)"
    );
}

/// recall_at with as_of > t2 must return M2 (the superseder) and not M1
/// (which was superseded at t2 and is no longer valid after that point).
#[tokio::test]
async fn recall_at_excludes_superseded_fact_after_supersession() {
    let (db, _dir) = test_db().await;

    let t1 = Utc::now() - Duration::days(30);
    let t2 = Utc::now() - Duration::days(10);
    let t3 = Utc::now() - Duration::days(1);

    let m1 = memory::create(&db,
        memory::MemoryDraft::new("Rome metro area population was old figure".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .event_at(t1))
    .await
    .unwrap();

    db.query("UPDATE type::record('memory', $id) SET created_at = $t1, event_at = $t1")
        .bind(("id", m1.as_str().to_string()))
        .bind(("t1", t1))
        .await
        .unwrap();

    // Supersede M1 at t2.
    memory::supersede(&db, &m1, t2).await.unwrap();

    let m2 = memory::create(&db,
        memory::MemoryDraft::new("Rome metro area population was updated new figure".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .event_at(t2))
    .await
    .unwrap();

    db.query("UPDATE type::record('memory', $id) SET created_at = $t2, event_at = $t2")
        .bind(("id", m2.as_str().to_string()))
        .bind(("t2", t2))
        .await
        .unwrap();

    // recall_at(as_of=t3): M1 was superseded at t2, so valid_until=t2 < t3 → excluded.
    // M2 was created at t2 ≤ t3 and still valid → included.
    let results = search::recall_at(
        &db,
        "Rome metro area population",
        None,
        None,
        t3,
        None,
        None,
        None,
        None,
        10,
        &AuthorFilter::None,
        &Default::default(),
        true,
    )
    .await
    .unwrap();

    let ids: Vec<&str> = results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        !ids.contains(&m1.as_str()),
        "M1 must be excluded after supersession: valid_until=t2 < t3"
    );
    assert!(
        ids.contains(&m2.as_str()),
        "M2 must be included at t3 (created at t2, still valid)"
    );
}

/// recall_at with as_of before any memory's created_at must return empty results.
#[tokio::test]
async fn recall_at_returns_nothing_before_memory_creation() {
    let (db, _dir) = test_db().await;

    let t0 = Utc::now() - Duration::days(10);
    let t1 = Utc::now() - Duration::days(5);

    let id = memory::create(&db,
        memory::MemoryDraft::new("memory created after the as_of point in time".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .event_at(t1))
    .await
    .unwrap();

    // Ensure created_at is also at t1 so the recall_at predicate holds.
    db.query("UPDATE type::record('memory', $id) SET created_at = $t1, event_at = $t1")
        .bind(("id", id.as_str().to_string()))
        .bind(("t1", t1))
        .await
        .unwrap();

    // as_of = t0 < t1: memory did not exist yet.
    let results = search::recall_at(
        &db,
        "memory created after the as_of point in time",
        None,
        None,
        t0,
        None,
        None,
        None,
        None,
        10,
        &AuthorFilter::None,
        &Default::default(),
        true,
    )
    .await
    .unwrap();

    let ids: Vec<&str> = results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        !ids.contains(&id.as_str()),
        "recall_at before memory creation must return empty"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Write-path reason propagation
// ═══════════════════════════════════════════════════════════════════════════

/// kb_forget must set invalidation_reason='forgotten' on the archived memory.
#[tokio::test]
async fn forget_sets_reason_forgotten() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "memory to forget because no longer relevant").await;

    archive::execute(
        &db,
        archive::ArchiveInput {
            id: id.as_str().to_string(),
            reason: smriti::types::ArchiveReason::Forget,
            correction_id: None,
        },
    )
    .await
    .unwrap();

    // Use raw query — get() filters to active only.
    let mut result = db
        .query("SELECT invalidation_reason FROM type::record('memory', $id)")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();
    let row: Option<serde_json::Value> = result.take(0).unwrap();
    let reason = row
        .as_ref()
        .and_then(|r| r.get("invalidation_reason"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    assert_eq!(reason, "forgotten", "kb_forget must set reason='forgotten'");
}

/// TTL expiry (prune::expire_ttl) must set invalidation_reason='expired'.
#[tokio::test]
async fn ttl_expiry_sets_reason_expired() {
    let (db, _dir) = test_db().await;

    let id = memory::create(&db,
        memory::MemoryDraft::new("scratch note that will expire via TTL prune".into(), MemoryType::Working, MemorySubtype::Scratch, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(1).unwrap()))
    .await
    .unwrap();

    // Deterministically move expires_at into the past.
    db.query("UPDATE type::record('memory', $id) SET expires_at = time::now() - 3h")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();

    prune::expire_ttl(&db).await.unwrap();

    // Raw query to inspect the archived record.
    let mut result = db
        .query("SELECT invalidation_reason, status FROM type::record('memory', $id)")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();
    let row: Option<serde_json::Value> = result.take(0).unwrap();
    let row = row.expect("record must exist after TTL archival");

    let reason = row
        .get("invalidation_reason")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let status = row
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    assert_eq!(reason, "expired", "TTL expiry must set reason='expired'");
    assert_eq!(status, "archived", "TTL expiry must set status='archived'");
}

/// kb_consolidate must set invalidation_reason='consolidated' on all source memories,
/// and the merged memory must inherit valid_until=max(sources) when all sources have it.
#[tokio::test]
async fn consolidation_sets_reason_consolidated_on_sources() {
    let (db, _dir) = test_db().await;

    let future_date = Utc::now() + Duration::days(90);
    let future_date_later = future_date + Duration::days(10);
    let t_early = Utc::now() - Duration::days(20);
    let t_later = Utc::now() - Duration::days(10);

    // Create source memories with explicit valid_until.
    let m1 = memory::create(&db,
        memory::MemoryDraft::new("observation about team velocity first measurement".into(), MemoryType::Semantic, MemorySubtype::Metric, Some(Channel::Cli), Some(Modality::Text))
            .project("proj-a".into())
            .importance(Importance::new(3).unwrap())
            .event_at(t_early)
            .valid_until(future_date))
    .await
    .unwrap();

    let m2 = memory::create(&db,
        memory::MemoryDraft::new("observation about team velocity second measurement".into(), MemoryType::Semantic, MemorySubtype::Metric, Some(Channel::Cli), Some(Modality::Text))
            .project("proj-a".into())
            .importance(Importance::new(3).unwrap())
            .event_at(t_later)
            .valid_until(future_date_later))
    .await
    .unwrap();

    let out = consolidate::execute(
        &db,
        None,
        consolidate::ConsolidateInput {
            ids: vec![m1.as_str().to_string(), m2.as_str().to_string()],
            result_summary: "consolidated velocity observation across two measurements".to_string(),
        },
    )
    .await
    .unwrap();

    // Verify both sources have reason='consolidated' and are archived.
    for (idx, src_id) in [&m1, &m2].iter().enumerate() {
        let mut result = db
            .query(
                "SELECT invalidation_reason, status, invalidated_at FROM type::record('memory', $id)",
            )
            .bind(("id", src_id.as_str().to_string()))
            .await
            .unwrap();
        let row: Option<serde_json::Value> = result.take(0).unwrap();
        let row = row
            .unwrap_or_else(|| panic!("source memory {idx} must exist after consolidation"));

        let reason = row
            .get("invalidation_reason")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let status = row
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let inv_at = row.get("invalidated_at");

        assert_eq!(
            reason, "consolidated",
            "source {idx} must have reason='consolidated'"
        );
        assert_eq!(
            status, "archived",
            "source {idx} must have status='archived'"
        );
        assert!(
            inv_at.is_some() && !inv_at.unwrap().is_null(),
            "source {idx} must have invalidated_at set"
        );
    }

    // The merged memory should have valid_until = max(m1.valid_until, m2.valid_until)
    // = future_date_later (the later of the two).
    let merged_id = MemoryId::from(out.id);
    let mut merged_result = db
        .query("SELECT valid_until, event_at FROM type::record('memory', $id)")
        .bind(("id", merged_id.as_str().to_string()))
        .await
        .unwrap();
    let merged_row: Option<serde_json::Value> = merged_result.take(0).unwrap();
    let merged_row = merged_row.expect("merged memory must exist");

    // valid_until must be set (both sources had valid_until).
    let valid_until = merged_row.get("valid_until");
    assert!(
        valid_until.is_some() && !valid_until.unwrap().is_null(),
        "merged memory must inherit valid_until when all sources have it set"
    );

    // event_at must equal min(source event_ats) = t_early.
    // Allow 1-second rounding tolerance for SurrealDB timestamp serialisation.
    let merged_event_at = merged_row
        .get("event_at")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<chrono::DateTime<Utc>>().ok())
        .expect("merged memory must have event_at");
    assert!(
        merged_event_at >= t_early - Duration::seconds(1)
            && merged_event_at <= t_early + Duration::seconds(1),
        "merged event_at must equal min(source event_ats)=t_early; got: {}",
        merged_event_at
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Consolidation valid_until semantics
// ═══════════════════════════════════════════════════════════════════════════

/// When any source has valid_until=None, the merged memory must have
/// valid_until=None (unknown end date).
#[tokio::test]
async fn consolidation_valid_until_null_when_any_source_null() {
    let (db, _dir) = test_db().await;

    let future = Utc::now() + Duration::days(60);

    // M1 has a valid_until; M2 does not.
    let m1 = memory::create(&db,
        memory::MemoryDraft::new("fact with known expiry date for consolidation test".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .project("proj-b".into())
            .importance(Importance::new(3).unwrap())
            .valid_until(future))
    .await
    .unwrap();

    let m2 = memory::create(&db,
        memory::MemoryDraft::new("fact with no expiry date for consolidation test merge".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .project("proj-b".into())
            .importance(Importance::new(3).unwrap()))
    .await
    .unwrap();

    let out = consolidate::execute(
        &db,
        None,
        consolidate::ConsolidateInput {
            ids: vec![m1.as_str().to_string(), m2.as_str().to_string()],
            result_summary:
                "merged fact without known expiry because one source had no expiry date".to_string(),
        },
    )
    .await
    .unwrap();

    let merged_id = MemoryId::from(out.id);
    let mut result = db
        .query("SELECT valid_until FROM type::record('memory', $id)")
        .bind(("id", merged_id.as_str().to_string()))
        .await
        .unwrap();
    let row: Option<serde_json::Value> = result.take(0).unwrap();
    let row = row.expect("merged memory must exist");

    let valid_until = row.get("valid_until");
    assert!(
        valid_until.is_none() || valid_until.unwrap().is_null(),
        "merged valid_until must be NULL when any source has valid_until=NULL"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// archive/retract set valid_until so point-in-time queries are fenced
// ═══════════════════════════════════════════════════════════════════════════

/// After archive (kb_forget), recall_at with as_of BEFORE the archive time must
/// show the memory (it was active then); recall_at with as_of AFTER the archive
/// time must NOT show it (valid_until = archive_time <= as_of).
///
/// This tests Phase 0-D: archive() sets `valid_until = time::now()` (guarded by
/// IS NONE) so the bitemporal predicate correctly fences point-in-time queries.
#[tokio::test]
async fn archive_sets_valid_until_for_bitemporal_fence() {
    let (db, _dir) = test_db().await;

    // T_past is well in the past; T_future is after the archive action (now).
    let t_past = Utc::now() - Duration::days(30);
    let t_future = Utc::now() + Duration::days(1);

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "ancient fact about Roman aqueducts construction era".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .event_at(t_past),
    )
    .await
    .unwrap();

    // Backdate created_at so recall_at(t_past) sees the memory as existing then.
    db.query("UPDATE type::record('memory', $id) SET created_at = $t, event_at = $t")
        .bind(("id", id.as_str().to_string()))
        .bind(("t", t_past))
        .await
        .unwrap();

    // Archive (forget) the memory — this must set valid_until = ~now.
    memory::archive(&db, &id, InvalidationReason::Forgotten)
        .await
        .unwrap();

    // Verify valid_until was set by the archive call.
    let mut r = db
        .query("SELECT valid_until FROM type::record('memory', $id)")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();
    let row: Option<serde_json::Value> = r.take(0).unwrap();
    let valid_until_set = row
        .as_ref()
        .and_then(|v| v.get("valid_until"))
        .map(|v| !v.is_null())
        .unwrap_or(false);
    assert!(
        valid_until_set,
        "archive() must set valid_until so bitemporal fence works"
    );

    // recall_at(as_of = t_past): memory existed and was valid then → must appear.
    let before_results = search::recall_at(
        &db,
        "ancient fact about Roman aqueducts construction era",
        None,
        None,
        t_past,
        None,
        None,
        None,
        None,
        10,
        &AuthorFilter::None,
        &Default::default(),
        true,
    )
    .await
    .unwrap();
    let before_ids: Vec<&str> = before_results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        before_ids.contains(&id.as_str()),
        "recall_at(t_past) must include memory that was active at t_past (before archive)"
    );

    // recall_at(as_of = t_future): valid_until was set to ~now ≤ t_future → excluded.
    let after_results = search::recall_at(
        &db,
        "ancient fact about Roman aqueducts construction era",
        None,
        None,
        t_future,
        None,
        None,
        None,
        None,
        10,
        &AuthorFilter::None,
        &Default::default(),
        true,
    )
    .await
    .unwrap();
    let after_ids: Vec<&str> = after_results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        !after_ids.contains(&id.as_str()),
        "recall_at(t_future) must exclude archived memory: valid_until <= t_future"
    );
}

/// After retract (kb_retract), the valid-time fence applies: in audit mode
/// (exclude_retracted=false), recall_at before retraction time shows the memory
/// and after does not. This verifies that retract() correctly sets valid_until.
///
/// With default exclude_retracted=true both queries return nothing — the retraction
/// filter takes precedence over the valid-time window, which is the expected behavior
/// per ADR-011 2026-05-01 update. The fence behavior is still tested via
/// exclude_retracted=false (audit mode).
#[tokio::test]
async fn retract_sets_valid_until_for_bitemporal_fence() {
    let (db, _dir) = test_db().await;

    let t_past = Utc::now() - Duration::days(30);
    let t_future = Utc::now() + Duration::days(1);

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "hypothesis about dark matter distribution in galactic haloes".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .event_at(t_past),
    )
    .await
    .unwrap();

    db.query("UPDATE type::record('memory', $id) SET created_at = $t, event_at = $t")
        .bind(("id", id.as_str().to_string()))
        .bind(("t", t_past))
        .await
        .unwrap();

    // Retract — must set valid_until = ~now (because valid_until was NONE).
    memory::retract(&db, &id, None).await.unwrap();

    // Audit mode (exclude_retracted=false): verify valid-time fence is correct.
    // recall_at(as_of = t_past): memory was active → must appear in audit mode.
    let before_results = search::recall_at(
        &db,
        "hypothesis about dark matter distribution in galactic haloes",
        None,
        None,
        t_past,
        None,
        None,
        None,
        None,
        10,
        &AuthorFilter::None,
        &Default::default(),
        false, // audit mode: see retracted memories
    )
    .await
    .unwrap();
    let before_ids: Vec<&str> = before_results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        before_ids.contains(&id.as_str()),
        "audit mode recall_at(t_past) must include retracted memory that was active at t_past"
    );

    // Audit mode: recall_at(as_of = t_future): valid_until = ~now ≤ t_future → excluded.
    let after_results = search::recall_at(
        &db,
        "hypothesis about dark matter distribution in galactic haloes",
        None,
        None,
        t_future,
        None,
        None,
        None,
        None,
        10,
        &AuthorFilter::None,
        &Default::default(),
        false, // audit mode
    )
    .await
    .unwrap();
    let after_ids: Vec<&str> = after_results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        !after_ids.contains(&id.as_str()),
        "recall_at(t_future) must exclude retracted memory: valid_until <= t_future"
    );

    // Default mode (exclude_retracted=true): retracted memory hidden at both time points.
    let default_results = search::recall_at(
        &db,
        "hypothesis about dark matter distribution in galactic haloes",
        None,
        None,
        t_past,
        None,
        None,
        None,
        None,
        10,
        &AuthorFilter::None,
        &Default::default(),
        true, // default: hide retracted
    )
    .await
    .unwrap();
    let default_ids: Vec<&str> = default_results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        !default_ids.contains(&id.as_str()),
        "default mode must hide retracted memory even at t_past (exclude_retracted=true)"
    );
}

/// When a caller has already set an explicit future valid_until, archive() must
/// NOT overwrite it — the IS NONE guard must hold.
#[tokio::test]
async fn archive_does_not_clobber_explicit_valid_until() {
    let (db, _dir) = test_db().await;

    let explicit_valid_until = Utc::now() + Duration::days(365);

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "contract clause valid for one full year from signing date".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .valid_until(explicit_valid_until),
    )
    .await
    .unwrap();

    // Archive with explicit valid_until already set — must NOT clobber it.
    memory::archive(&db, &id, InvalidationReason::Forgotten)
        .await
        .unwrap();

    let mut r = db
        .query("SELECT valid_until FROM type::record('memory', $id)")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();
    let row: Option<serde_json::Value> = r.take(0).unwrap();
    let stored_valid_until = row
        .as_ref()
        .and_then(|v| v.get("valid_until"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<chrono::DateTime<Utc>>().ok())
        .expect("valid_until must still be set after archive");

    // Allow 2-second tolerance for test clock skew.
    let diff = (stored_valid_until - explicit_valid_until).num_seconds().abs();
    assert!(
        diff <= 2,
        "archive() must not clobber an explicitly-set valid_until; \
         expected ~{explicit_valid_until}, got {stored_valid_until}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Consolidation bitemporal fence — ADR-011 Change 1
// ═══════════════════════════════════════════════════════════════════════════

/// After consolidation, `recall_at(as_of=T0)` must return the original sources
/// (they were active then); `recall_at(as_of=T1)` must return only the merged
/// memory (sources have valid_until set to consolidation time).
///
/// This validates that `consolidate_atomic` sets `valid_until` on source memories,
/// mirroring `archive()`, so the bitemporal predicate `valid_until > $as_of`
/// correctly fences point-in-time queries.
#[tokio::test]
async fn consolidate_then_recall_at_surfaces_pre_merge_sources() {
    let (db, _dir) = test_db().await;

    let t0 = Utc::now() - chrono::Duration::days(30);
    // t1 is strictly after the consolidation call (which happens ~now).
    let t1 = Utc::now() + chrono::Duration::days(1);

    // Create 3 source memories and backdate their created_at to t0.
    let mut source_ids = Vec::new();
    let summaries = [
        "ancient source fact alpha about the consolidation merge test",
        "ancient source fact beta about the consolidation merge test",
        "ancient source fact gamma about the consolidation merge test",
    ];
    for summary in summaries {
        let id = memory::create(
            &db,
            memory::MemoryDraft::new(
                summary.to_string(),
                MemoryType::Semantic,
                MemorySubtype::Fact,
                Some(Channel::Cli),
                Some(Modality::Text),
            )
            .project("bitemporal-consolidation-test".into())
            .importance(Importance::new(3).unwrap())
            .event_at(t0),
        )
        .await
        .unwrap();

        // Backdate created_at so recall_at(t0) sees the memory as existing.
        db.query("UPDATE type::record('memory', $id) SET created_at = $t")
            .bind(("id", id.as_str().to_string()))
            .bind(("t", t0))
            .await
            .unwrap();

        source_ids.push(id);
    }

    // Consolidate the 3 sources now (at ~T_consolidation, which is between t0 and t1).
    let out = consolidate::execute(
        &db,
        None,
        consolidate::ConsolidateInput {
            ids: source_ids.iter().map(|id| id.as_str().to_string()).collect(),
            result_summary: "merged ancient source facts about consolidation merge test combined"
                .to_string(),
        },
    )
    .await
    .unwrap();

    let merged_id = MemoryId::from(out.id);

    // recall_at(as_of=t0): sources existed and were active → all 3 must appear.
    let before_results = search::recall_at(
        &db,
        "ancient source fact about consolidation merge test",
        None,
        None,
        t0,
        Some("bitemporal-consolidation-test"),
        None,
        None,
        None,
        20,
        &AuthorFilter::None,
        &Default::default(),
        true,
    )
    .await
    .unwrap();
    let before_ids: Vec<&str> = before_results.iter().map(|m| m.id.as_str()).collect();

    for src_id in &source_ids {
        assert!(
            before_ids.contains(&src_id.as_str()),
            "recall_at(t0) must include source {src_id} — it was active at t0 before consolidation"
        );
    }

    // recall_at(as_of=t1): sources have valid_until=~consolidation_time ≤ t1
    // → sources excluded. Merged memory was created at ~now, so created_at ≤ t1.
    let after_results = search::recall_at(
        &db,
        "merged ancient source facts about consolidation merge test",
        None,
        None,
        t1,
        Some("bitemporal-consolidation-test"),
        None,
        None,
        None,
        20,
        &AuthorFilter::None,
        &Default::default(),
        true,
    )
    .await
    .unwrap();
    let after_ids: Vec<&str> = after_results.iter().map(|m| m.id.as_str()).collect();

    for src_id in &source_ids {
        assert!(
            !after_ids.contains(&src_id.as_str()),
            "recall_at(t1) must exclude source {src_id} — valid_until was set at consolidation time"
        );
    }

    assert!(
        after_ids.contains(&merged_id.as_str()),
        "recall_at(t1) must include the merged memory (created at consolidation time)"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// exclude_retracted filter — default behavior and opt-out
// ═══════════════════════════════════════════════════════════════════════════

/// With exclude_retracted=true (default), recall_at returns nothing for a
/// memory that was retracted, even if as_of falls inside its valid-time window.
///
/// Setup: memory created and backdated to t1; retracted at t2; as_of = t1.5
/// (strictly between t1 and t2). The valid-time window covers t1.5 but the
/// memory was marked wrong — default flag must hide it.
#[tokio::test]
async fn recall_at_default_excludes_retracted_within_valid_window() {
    let (db, _dir) = test_db().await;

    let t1 = Utc::now() - Duration::days(20);
    let t2 = Utc::now() - Duration::days(5);
    let t_mid = Utc::now() - Duration::days(12); // strictly between t1 and t2

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "incorrect hypothesis about quantum tunnelling barrier thickness".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli),
            Some(Modality::Text),
        )
        .event_at(t1),
    )
    .await
    .unwrap();

    // Backdate created_at so recall_at(t_mid) sees the memory as existing then.
    db.query("UPDATE type::record('memory', $id) SET created_at = $t1, event_at = $t1")
        .bind(("id", id.as_str().to_string()))
        .bind(("t1", t1))
        .await
        .unwrap();

    // Retract at t2: sets invalidated_at=~now and valid_until=~now.
    // Then manually backdate valid_until to t2 so t_mid < t2.
    memory::retract(&db, &id, None).await.unwrap();
    db.query("UPDATE type::record('memory', $id) SET valid_until = $t2")
        .bind(("id", id.as_str().to_string()))
        .bind(("t2", t2))
        .await
        .unwrap();

    // Default flag (true): memory must NOT appear even though t_mid is inside [t1, t2).
    let results = search::recall_at(
        &db,
        "incorrect hypothesis about quantum tunnelling barrier thickness",
        None,
        None,
        t_mid,
        None,
        None,
        None,
        None,
        10,
        &AuthorFilter::None,
        &Default::default(),
        true,
    )
    .await
    .unwrap();

    let ids: Vec<&str> = results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        !ids.contains(&id.as_str()),
        "exclude_retracted=true must hide retracted memory even when as_of is within valid window"
    );
}

/// With exclude_retracted=false, recall_at returns a retracted memory when
/// as_of falls inside its valid-time window (forensic/audit mode).
#[tokio::test]
async fn recall_at_exclude_retracted_false_returns_retracted_in_valid_window() {
    let (db, _dir) = test_db().await;

    let t1 = Utc::now() - Duration::days(20);
    let t2 = Utc::now() - Duration::days(5);
    let t_mid = Utc::now() - Duration::days(12);

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "retracted claim about dark energy density measurement audit test".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli),
            Some(Modality::Text),
        )
        .event_at(t1),
    )
    .await
    .unwrap();

    db.query("UPDATE type::record('memory', $id) SET created_at = $t1, event_at = $t1")
        .bind(("id", id.as_str().to_string()))
        .bind(("t1", t1))
        .await
        .unwrap();

    memory::retract(&db, &id, None).await.unwrap();
    // Backdate valid_until to t2 so t_mid < t2 (memory was "valid" during [t1, t2)).
    db.query("UPDATE type::record('memory', $id) SET valid_until = $t2")
        .bind(("id", id.as_str().to_string()))
        .bind(("t2", t2))
        .await
        .unwrap();

    // Audit mode (false): retracted memory MUST appear at t_mid.
    let results = search::recall_at(
        &db,
        "retracted claim about dark energy density measurement audit test",
        None,
        None,
        t_mid,
        None,
        None,
        None,
        None,
        10,
        &AuthorFilter::None,
        &Default::default(),
        false,
    )
    .await
    .unwrap();

    let ids: Vec<&str> = results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        ids.contains(&id.as_str()),
        "exclude_retracted=false must return retracted memory when as_of is within valid window"
    );
}

/// Superseded (not retracted) memories are NOT hidden by exclude_retracted=true.
/// Only InvalidationReason::Retracted is filtered; other reasons pass through.
#[tokio::test]
async fn recall_at_default_does_not_filter_superseded_memories() {
    let (db, _dir) = test_db().await;

    let t1 = Utc::now() - Duration::days(20);
    let t2 = Utc::now() - Duration::days(5);
    let t_mid = Utc::now() - Duration::days(12);

    // Create a memory and supersede it (not retract) — supersession represents
    // "the fact changed", not "I was wrong".
    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "superseded fact about tectonic plate movement annual rate estimate".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli),
            Some(Modality::Text),
        )
        .event_at(t1),
    )
    .await
    .unwrap();

    db.query("UPDATE type::record('memory', $id) SET created_at = $t1, event_at = $t1")
        .bind(("id", id.as_str().to_string()))
        .bind(("t1", t1))
        .await
        .unwrap();

    // Supersede at t2: sets valid_until=t2.
    memory::supersede(&db, &id, t2).await.unwrap();

    // recall_at(t_mid) with default exclude_retracted=true: superseded memory
    // must still appear because t_mid < t2 (valid_until not yet reached).
    let results = search::recall_at(
        &db,
        "superseded fact about tectonic plate movement annual rate estimate",
        None,
        None,
        t_mid,
        None,
        None,
        None,
        None,
        10,
        &AuthorFilter::None,
        &Default::default(),
        true,
    )
    .await
    .unwrap();

    let ids: Vec<&str> = results.iter().map(|m| m.id.as_str()).collect();
    assert!(
        ids.contains(&id.as_str()),
        "exclude_retracted=true must NOT hide superseded memories — only retracted ones are filtered"
    );
}
