mod common;
use common::{test_db, create_test_memory};

use chrono::{Duration, Utc};
use smriti::db::{memory, prune};
use smriti::config::PruningConfig;
use smriti::types::*;

// ═══════════════════════════════════════════════════════════════════════════
// PRUNING
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn ttl_expiry_archives_expired_memories() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db, "expired scratch".into(), None,
        MemoryType::Working, MemorySubtype::Scratch, Source::Manual,
        None, vec![], vec![], Importance::default(), Visibility::default(), None, vec![],
        Some(Utc::now() - Duration::hours(1)), None, None,
    ).await.unwrap();

    let future_id = memory::create(
        &db, "future scratch".into(), None,
        MemoryType::Working, MemorySubtype::Scratch, Source::Manual,
        None, vec![], vec![], Importance::default(), Visibility::default(), None, vec![],
        Some(Utc::now() + Duration::days(30)), None, None,
    ).await.unwrap();

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

    let id = memory::create(
        &db, "unaccessed memory".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::new(2).unwrap(), Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    db.query("UPDATE type::record('memory', $id) SET updated_at = time::now() - 2d")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();

    prune::run_all(&db, &config).await.unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    assert_eq!(mem.status, MemoryStatus::Archived);
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

    let id = memory::create(
        &db, "recently reviewed".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::new(3).unwrap(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

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

    let id = memory::create(
        &db, "overdue memory".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::new(4).unwrap(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

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
    let id = memory::create(
        &db, "forgotten memory".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::new(4).unwrap(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

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
    let id = memory::create(
        &db, "hard to remember".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::new(4).unwrap(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

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

    let id = memory::create(
        &db, "pre-migration memory".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::new(3).unwrap(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    // Ensure NO sm2 fields, old updated_at
    db.query("UPDATE type::record('memory', $id) SET next_review_at = NONE, ease_factor = NONE, updated_at = time::now() - 2d")
        .bind(("id", id.as_str().to_string()))
        .await
        .unwrap();

    prune::sm2_decay(&db, &config).await.unwrap();

    let mem = memory::get(&db, &id).await.unwrap();
    assert_eq!(mem.importance.value(), 2, "legacy decay should reduce importance by 1");
}
