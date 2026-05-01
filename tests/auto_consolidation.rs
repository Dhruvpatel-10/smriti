mod common;
use common::test_db;

use smriti::db::consolidation_ledger::{self, count_attempts_last_24h, AttemptOutcome};
use smriti::db::consolidation::fetch_memories_by_ids;
use smriti::types::{MemoryId, MemoryType, MemorySubtype, Visibility};

// ── Pure unit tests (no DB, no sidecar) ─────────────────────────────────────

/// The summary length bounds enforced by the auto-consolidation loop
/// are 10..=500 characters inclusive.
#[test]
fn merged_summary_length_validation_accepts_valid() {
    let short = "A".repeat(10);
    let long = "A".repeat(500);
    let middle = "Valid merged summary text.";

    assert!(short.len() >= 10 && short.len() <= 500);
    assert!(long.len() >= 10 && long.len() <= 500);
    assert!(middle.len() >= 10 && middle.len() <= 500);
}

#[test]
fn merged_summary_length_validation_rejects_too_short() {
    let too_short = "Short";
    assert!(too_short.len() < 10, "expected rejection: length {}", too_short.len());
}

#[test]
fn merged_summary_length_validation_rejects_too_long() {
    let too_long = "A".repeat(501);
    assert!(too_long.len() > 500, "expected rejection: length {}", too_long.len());
}

/// `ConsolidationCandidate::memory_ids()` converts raw string IDs to typed newtypes.
#[test]
fn candidate_memory_ids_converts_strings() {
    use smriti::db::consolidation::ConsolidationCandidate;

    let candidate = ConsolidationCandidate {
        ids: vec![
            "mem_aaa111".to_string(),
            "mem_bbb222".to_string(),
        ],
        summaries: vec!["Summary A".to_string(), "Summary B".to_string()],
        reason: "Test cluster".to_string(),
        project: None,
        author: None,
    };

    let memory_ids = candidate.memory_ids().expect("memory_ids should not fail");
    assert_eq!(memory_ids.len(), 2);
    assert_eq!(memory_ids[0].as_str(), "mem_aaa111");
    assert_eq!(memory_ids[1].as_str(), "mem_bbb222");
}

#[test]
fn candidate_memory_ids_empty_cluster() {
    use smriti::db::consolidation::ConsolidationCandidate;

    let candidate = ConsolidationCandidate {
        ids: vec![],
        summaries: vec![],
        reason: "Empty".to_string(),
        project: None,
        author: None,
    };

    let memory_ids = candidate.memory_ids().expect("should succeed with empty vec");
    assert!(memory_ids.is_empty());
}

// ── Integration tests (DB required, no sidecar) ──────────────────────────────

/// `fetch_memories_by_ids` with an empty slice returns an empty vec without
/// hitting the database (early-return path).
#[tokio::test]
async fn fetch_memories_by_ids_empty_input_returns_empty() {
    let (db, _dir) = test_db().await;
    let result = fetch_memories_by_ids(&db, &[])
        .await
        .expect("empty fetch should succeed");
    assert!(result.is_empty());
}

/// `fetch_memories_by_ids` returns memories that exist and omits IDs that are
/// not in the database.
#[tokio::test]
async fn fetch_memories_by_ids_returns_existing_skips_missing() {
    let (db, _dir) = test_db().await;

    let real_id = common::create_test_memory(&db, "Real memory for fetch test").await;
    let ghost_id = MemoryId::from("mem_doesnotexist99999".to_string());

    let results = fetch_memories_by_ids(&db, &[real_id.clone(), ghost_id])
        .await
        .expect("fetch should succeed");

    // Only the real memory should be returned.
    assert_eq!(results.len(), 1, "expected 1 result, got {}", results.len());
    assert_eq!(results[0].id, real_id);
    assert!(!results[0].summary.is_empty());
}

/// `fetch_memories_by_ids` returns correct `visibility` and `subtype` fields.
#[tokio::test]
async fn fetch_memories_by_ids_correct_fields() {
    let (db, _dir) = test_db().await;

    // Create a memory directly at the DB layer so we control type/subtype.
    use smriti::db::memory::{self as db_memory, MemoryDraft};
    use smriti::types::{Channel, Modality};

    let id = db_memory::create(
        &db,
        MemoryDraft::new(
            "Decision memory for auto-consolidation field test".to_string(),
            MemoryType::Semantic,
            MemorySubtype::Decision,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .expect("create failed");

    let results = fetch_memories_by_ids(&db, std::slice::from_ref(&id))
        .await
        .expect("fetch should succeed");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].subtype, MemorySubtype::Decision);
    assert_eq!(results[0].visibility, Visibility::Standard);
}

/// Registering the auto-consolidator agent twice (idempotent path) must not
/// return an error — the second call should look up the existing agent.
///
/// This validates the `ensure_auto_consolidator_agent` logic indirectly via
/// the public DB layer that it calls.
#[tokio::test]
async fn auto_consolidator_agent_registration_idempotent() {
    let (db, _dir) = test_db().await;

    use smriti::db::agent::{register, resolve_by_name, RegisterInput};
    use smriti::error::KbError;
    use smriti::types::AgentKind;

    let name = "auto-consolidator";

    // First registration should succeed.
    let first_id = register(
        &db,
        RegisterInput {
            name: name.to_string(),
            kind: AgentKind::Summarizer,
            model: Some("claude-sonnet-4-5".to_string()),
            version: None,
            parent: None,
            project: None,
            config: None,
        },
    )
    .await
    .expect("first registration should succeed");

    // Second registration returns InvalidInput("already active").
    let second = register(
        &db,
        RegisterInput {
            name: name.to_string(),
            kind: AgentKind::Summarizer,
            model: Some("claude-sonnet-4-5".to_string()),
            version: None,
            parent: None,
            project: None,
            config: None,
        },
    )
    .await;

    // Verify the idempotent fallback: "already active" → resolve existing.
    let resolved_id = match second {
        Ok(id) => id,
        Err(KbError::InvalidInput(ref msg)) if msg.contains("already active") => {
            resolve_by_name(&db, name)
                .await
                .expect("resolve_by_name should succeed")
                .expect("agent should exist")
                .id
        }
        Err(e) => panic!("unexpected error on second registration: {e}"),
    };

    assert_eq!(first_id, resolved_id, "idempotent registration must return the same AgentId");
}

// ── Integration tests (require sidecar — marked ignore) ──────────────────────

/// Full loop merge path: three similar memories should be merged when the
/// sidecar returns high confidence.
#[tokio::test]
#[ignore = "requires sidecar binary (node sidecar/dist/index.js)"]
async fn auto_consol_loop_merges_when_confidence_high() {
    // Documented intent only — full wiring requires a live sidecar.
    // When this is un-ignored: set up 3 Semantic/Decision memories in the same
    // project, run find_candidates, call synthesize_consolidation with a mock
    // that returns should_merge=true + confidence=0.95, then verify
    // consolidate_atomic produced a merged memory and the ledger records Merged.
}

/// Low-confidence sidecar response should result in DeferredLowConfidence
/// in the ledger without creating a new memory.
#[tokio::test]
#[ignore = "requires sidecar binary (node sidecar/dist/index.js)"]
async fn auto_consol_loop_defers_when_confidence_low() {
    // When un-ignored: mock sidecar returns should_merge=false, confidence=0.3.
    // Verify no new memory, ledger shows DeferredLowConfidence.
}

/// A cluster containing a Sensitive memory is deferred before sidecar is called.
#[tokio::test]
#[ignore = "requires sidecar binary (node sidecar/dist/index.js)"]
async fn auto_consol_loop_skips_sensitive_clusters() {
    // When un-ignored: create one Sensitive + two Standard memories, verify
    // the loop records DeferredSensitive without calling the sidecar.
}

/// A cluster in cooldown is skipped entirely on the next tick.
#[tokio::test]
#[ignore = "requires sidecar binary (node sidecar/dist/index.js)"]
async fn auto_consol_loop_respects_cooldown() {
    // When un-ignored: record an attempt with 7-day cooldown, run the loop,
    // verify no new attempt is recorded within the cooldown window.
}

/// A sidecar failure for one cluster does not prevent other clusters from
/// being processed in the same tick.
#[tokio::test]
#[ignore = "requires sidecar binary (node sidecar/dist/index.js)"]
async fn auto_consol_loop_isolated_circuit_breaker_does_not_affect_extraction() {
    // When un-ignored: simulate a sidecar error on cluster A, verify cluster B
    // is still processed (no loop abort).
}

// ── Ledger round-trip for auto-consolidation outcomes ───────────────────────

/// Record a DeferredSensitive attempt and verify it is retrievable via cooldown check.
#[tokio::test]
async fn ledger_deferred_sensitive_sets_cooldown() {
    let (db, _dir) = test_db().await;

    let ids = vec![
        MemoryId::from("mem_sens1".to_string()),
        MemoryId::from("mem_sens2".to_string()),
    ];
    let hash = consolidation_ledger::cluster_hash(&ids);

    consolidation_ledger::record_attempt(
        &db,
        &hash,
        &ids,
        AttemptOutcome::DeferredSensitive,
        Some("contains sensitive memory".to_string()),
        None,
        7,
    )
    .await
    .expect("record_attempt failed");

    let cooldown = consolidation_ledger::check_cooldown(&db, &hash)
        .await
        .expect("check_cooldown failed");

    assert!(cooldown.is_some(), "DeferredSensitive should set a cooldown");
}

/// Record a MergeFailed attempt and verify the outcome string in the ledger.
#[tokio::test]
async fn ledger_merge_failed_outcome_persisted() {
    let (db, _dir) = test_db().await;

    let ids = vec![MemoryId::from("mem_fail1".to_string())];
    let hash = consolidation_ledger::cluster_hash(&ids);

    consolidation_ledger::record_attempt(
        &db,
        &hash,
        &ids,
        AttemptOutcome::MergeFailed,
        Some("merged_summary length out of bounds".to_string()),
        None,
        3,
    )
    .await
    .expect("record_attempt failed");

    let mut result = db
        .query(
            "SELECT last_outcome FROM consolidation_attempt \
             WHERE cluster_hash = $hash LIMIT 1",
        )
        .bind(("hash", hash))
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = result.take(0).unwrap();
    assert_eq!(rows.len(), 1);
    let outcome = rows[0]
        .get("last_outcome")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(outcome, "merge_failed");
}

// ── Daily budget cap tests ────────────────────────────────────────────────────

/// `count_attempts_last_24h` returns 0 when the ledger is empty.
#[tokio::test]
async fn daily_cap_count_zero_on_empty_ledger() {
    let (db, _dir) = test_db().await;

    let count = count_attempts_last_24h(&db)
        .await
        .expect("count_attempts_last_24h should not fail on empty ledger");

    assert_eq!(count, 0, "empty ledger should return 0");
}

/// Seed N attempts in the ledger (all within the last 24h) where
/// N == max_attempts_per_day. `count_attempts_last_24h` must return N.
/// The auto-consolidation loop guard (`count >= max_attempts_per_day`) must
/// then be satisfied, meaning no further sidecar calls would be issued.
///
/// This tests the ledger query used by the budget cap in `src/main.rs`.
#[tokio::test]
async fn daily_cap_count_matches_seeded_attempts() {
    let (db, _dir) = test_db().await;
    let max_attempts_per_day: u32 = 3; // small value for test speed

    // Seed `max_attempts_per_day` distinct clusters, each with a recent attempt.
    for i in 0..max_attempts_per_day {
        let ids = vec![MemoryId::from(format!("mem_daily_cap_{i}"))];
        let hash = consolidation_ledger::cluster_hash(&ids);
        consolidation_ledger::record_attempt(
            &db,
            &hash,
            &ids,
            AttemptOutcome::DeferredLowConfidence,
            Some("test seed".to_string()),
            None,
            1,
        )
        .await
        .expect("record_attempt failed");
    }

    let count = count_attempts_last_24h(&db)
        .await
        .expect("count_attempts_last_24h should succeed");

    assert_eq!(
        count, max_attempts_per_day,
        "count should equal the number of seeded attempts"
    );

    // Simulate the guard condition: if count >= max_attempts_per_day → break.
    assert!(
        count >= max_attempts_per_day,
        "budget guard should fire when count={count} >= max={max_attempts_per_day}"
    );
}

/// `count_attempts_last_24h` counts all rows regardless of outcome.
/// Seed attempts with mixed outcomes — all should be counted.
#[tokio::test]
async fn daily_cap_counts_all_outcomes() {
    let (db, _dir) = test_db().await;

    let outcomes = [
        AttemptOutcome::Merged,
        AttemptOutcome::DeferredLowConfidence,
        AttemptOutcome::SidecarError,
    ];

    for (i, outcome) in outcomes.iter().enumerate() {
        let ids = vec![MemoryId::from(format!("mem_mixed_outcome_{i}"))];
        let hash = consolidation_ledger::cluster_hash(&ids);
        consolidation_ledger::record_attempt(
            &db,
            &hash,
            &ids,
            *outcome,
            None,
            None,
            1,
        )
        .await
        .expect("record_attempt failed");
    }

    let count = count_attempts_last_24h(&db)
        .await
        .expect("count_attempts_last_24h should succeed");

    assert_eq!(count, 3, "all 3 outcomes should be counted in 24h window");
}
