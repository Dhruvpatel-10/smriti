mod common;
use common::test_db;

use smriti::db::consolidation_ledger::{self, AttemptOutcome};
use smriti::types::MemoryId;

// ── Pure unit tests (no DB) ───────────────────────────────────────────────────

#[test]
fn cluster_hash_is_order_independent() {
    let a = MemoryId::from("mem_aaa".to_string());
    let b = MemoryId::from("mem_bbb".to_string());
    let c = MemoryId::from("mem_ccc".to_string());

    let h1 = consolidation_ledger::cluster_hash(&[a.clone(), b.clone(), c.clone()]);
    let h2 = consolidation_ledger::cluster_hash(&[c.clone(), a.clone(), b.clone()]);
    let h3 = consolidation_ledger::cluster_hash(&[b, c, a]);

    assert_eq!(h1, h2, "permutation 1 != permutation 2");
    assert_eq!(h1, h3, "permutation 1 != permutation 3");
}

#[test]
fn cluster_hash_differs_for_different_clusters() {
    let a = MemoryId::from("mem_aaa".to_string());
    let b = MemoryId::from("mem_bbb".to_string());
    let c = MemoryId::from("mem_ccc".to_string());

    let h_ab = consolidation_ledger::cluster_hash(&[a.clone(), b.clone()]);
    let h_abc = consolidation_ledger::cluster_hash(&[a, b, c]);

    assert_ne!(h_ab, h_abc, "different clusters must produce different hashes");
}

// ── Integration tests (DB required) ──────────────────────────────────────────

#[tokio::test]
async fn record_attempt_inserts_new_row() {
    let (db, _dir) = test_db().await;

    let ids = vec![
        MemoryId::from("mem_x1".to_string()),
        MemoryId::from("mem_x2".to_string()),
    ];
    let hash = consolidation_ledger::cluster_hash(&ids);

    consolidation_ledger::record_attempt(
        &db,
        &hash,
        &ids,
        AttemptOutcome::DeferredLowConfidence,
        Some("below 0.7 threshold".to_string()),
        None,
        7,
    )
    .await
    .expect("record_attempt failed");

    // Verify via raw query: row must exist with attempt_count = 1.
    let mut result = db
        .query("SELECT attempt_count, last_outcome FROM consolidation_attempt WHERE cluster_hash = $hash LIMIT 1")
        .bind(("hash", hash.clone()))
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = result.take(0).unwrap();
    assert_eq!(rows.len(), 1, "expected one row after first insert");

    let count = rows[0]
        .get("attempt_count")
        .and_then(|v| v.as_u64())
        .expect("attempt_count missing");
    assert_eq!(count, 1, "first insert should have attempt_count=1");

    let outcome = rows[0]
        .get("last_outcome")
        .and_then(|v| v.as_str())
        .expect("last_outcome missing");
    assert_eq!(outcome, "deferred_low_confidence");
}

#[tokio::test]
async fn record_attempt_upserts_on_conflict() {
    let (db, _dir) = test_db().await;

    let ids = vec![
        MemoryId::from("mem_y1".to_string()),
        MemoryId::from("mem_y2".to_string()),
    ];
    let hash = consolidation_ledger::cluster_hash(&ids);

    // First attempt.
    consolidation_ledger::record_attempt(
        &db,
        &hash,
        &ids,
        AttemptOutcome::SidecarError,
        None,
        None,
        3,
    )
    .await
    .unwrap();

    // Second attempt — same hash, different outcome.
    consolidation_ledger::record_attempt(
        &db,
        &hash,
        &ids,
        AttemptOutcome::Merged,
        None,
        Some(MemoryId::from("mem_merged".to_string())),
        1,
    )
    .await
    .unwrap();

    let mut result = db
        .query("SELECT attempt_count, last_outcome FROM consolidation_attempt WHERE cluster_hash = $hash LIMIT 1")
        .bind(("hash", hash))
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = result.take(0).unwrap();
    assert_eq!(rows.len(), 1, "upsert must not create a second row");

    let count = rows[0]
        .get("attempt_count")
        .and_then(|v| v.as_u64())
        .expect("attempt_count missing");
    assert_eq!(count, 2, "second call should increment attempt_count to 2");

    let outcome = rows[0]
        .get("last_outcome")
        .and_then(|v| v.as_str())
        .expect("last_outcome missing");
    assert_eq!(outcome, "merged", "last_outcome must reflect the latest call");
}

#[tokio::test]
async fn check_cooldown_returns_none_when_no_attempt() {
    let (db, _dir) = test_db().await;

    let ids = vec![MemoryId::from("mem_z1".to_string())];
    let hash = consolidation_ledger::cluster_hash(&ids);

    let result = consolidation_ledger::check_cooldown(&db, &hash)
        .await
        .expect("check_cooldown failed");

    assert!(result.is_none(), "no attempt recorded → should be None");
}

#[tokio::test]
async fn check_cooldown_returns_some_within_cooldown_window() {
    let (db, _dir) = test_db().await;

    let ids = vec![
        MemoryId::from("mem_w1".to_string()),
        MemoryId::from("mem_w2".to_string()),
    ];
    let hash = consolidation_ledger::cluster_hash(&ids);

    consolidation_ledger::record_attempt(
        &db,
        &hash,
        &ids,
        AttemptOutcome::DeferredOversized,
        None,
        None,
        7, // 7-day cooldown
    )
    .await
    .unwrap();

    let result = consolidation_ledger::check_cooldown(&db, &hash)
        .await
        .expect("check_cooldown failed");

    assert!(
        result.is_some(),
        "just-recorded attempt with 7-day cooldown should block immediately"
    );
}

#[tokio::test]
async fn check_cooldown_returns_none_after_window_expires() {
    let (db, _dir) = test_db().await;

    let ids = vec![MemoryId::from("mem_v1".to_string())];
    let hash = consolidation_ledger::cluster_hash(&ids);

    // Insert a row manually with skip_until in the past.
    let past = chrono::Utc::now() - chrono::Duration::days(1);
    db.query(
        "CREATE consolidation_attempt SET
             cluster_hash  = $hash,
             source_ids    = [],
             attempted_at  = $past,
             last_outcome  = 'deferred_oversized',
             last_reason   = NONE,
             merged_into   = NONE,
             skip_until    = $past,
             attempt_count = 1",
    )
    .bind(("hash", hash.clone()))
    .bind(("past", past))
    .await
    .unwrap();

    let result = consolidation_ledger::check_cooldown(&db, &hash)
        .await
        .expect("check_cooldown failed");

    assert!(
        result.is_none(),
        "expired skip_until should return None (cooldown over)"
    );
}
