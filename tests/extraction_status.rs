use smriti::config::AlertConfig;
use smriti::db::{alerts, memory};
use smriti::types::*;

mod common;
use common::test_db;

// ═══════════════════════════════════════════════════════════════════════════
// extraction_status field: initial state and transitions
// ═══════════════════════════════════════════════════════════════════════════

/// New memories created via `memory::create` must have extraction_status = 'pending'.
#[tokio::test]
async fn new_memory_has_pending_extraction_status() {
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Decided to use SurrealDB for knowledge storage".into(),
            MemoryType::Semantic,
            MemorySubtype::Decision,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .expect("create");

    let mem = memory::get(&db, &id).await.expect("get");
    assert_eq!(
        mem.extraction_status,
        Some(ExtractionStatus::Pending),
        "extraction_status must be Pending immediately after create"
    );
}

/// set_extraction_status transitions from pending → complete.
#[tokio::test]
async fn set_extraction_status_pending_to_complete() {
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Rust is the implementation language for smriti".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .expect("create");

    memory::set_extraction_status(&db, &id, ExtractionStatus::Complete)
        .await
        .expect("set_extraction_status");

    let mem = memory::get(&db, &id).await.expect("get");
    assert_eq!(mem.extraction_status, Some(ExtractionStatus::Complete));
}

/// set_extraction_status transitions from pending → anchor_failed.
#[tokio::test]
async fn set_extraction_status_anchor_failed() {
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Sarah delivered the gRPC interceptor rollout on schedule".into(),
            MemoryType::Episodic,
            MemorySubtype::Meeting,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .expect("create");

    memory::set_extraction_status(&db, &id, ExtractionStatus::AnchorFailed)
        .await
        .expect("set_extraction_status");

    let mem = memory::get(&db, &id).await.expect("get");
    assert_eq!(mem.extraction_status, Some(ExtractionStatus::AnchorFailed));
}

/// set_extraction_status transitions from pending → relation_failed.
#[tokio::test]
async fn set_extraction_status_relation_failed() {
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Decided on React for the frontend framework choice".into(),
            MemoryType::Semantic,
            MemorySubtype::Decision,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .expect("create");

    memory::set_extraction_status(&db, &id, ExtractionStatus::RelationFailed)
        .await
        .expect("set_extraction_status");

    let mem = memory::get(&db, &id).await.expect("get");
    assert_eq!(mem.extraction_status, Some(ExtractionStatus::RelationFailed));
}

/// set_extraction_status transitions from pending → both_failed.
#[tokio::test]
async fn set_extraction_status_both_failed() {
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "The team agreed to adopt the new design system guidelines".into(),
            MemoryType::Episodic,
            MemorySubtype::Meeting,
            Some(Channel::Slack),
            Some(Modality::Text),
        ),
    )
    .await
    .expect("create");

    memory::set_extraction_status(&db, &id, ExtractionStatus::BothFailed)
        .await
        .expect("set_extraction_status");

    let mem = memory::get(&db, &id).await.expect("get");
    assert_eq!(mem.extraction_status, Some(ExtractionStatus::BothFailed));
}

/// set_extraction_status transitions from pending → sidecar_unavailable.
#[tokio::test]
async fn set_extraction_status_sidecar_unavailable() {
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Need to migrate user authentication to OAuth2 provider".into(),
            MemoryType::Prospective,
            MemorySubtype::ActionItem,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .expect("create");

    memory::set_extraction_status(&db, &id, ExtractionStatus::SidecarUnavailable)
        .await
        .expect("set_extraction_status");

    let mem = memory::get(&db, &id).await.expect("get");
    assert_eq!(
        mem.extraction_status,
        Some(ExtractionStatus::SidecarUnavailable)
    );
}

/// Once complete, set_extraction_status must NOT overwrite with a failure status.
/// This guards the retry-then-race scenario.
#[tokio::test]
async fn set_extraction_status_does_not_overwrite_complete() {
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Kubernetes cluster scaled to handle peak traffic load successfully".into(),
            MemoryType::Episodic,
            MemorySubtype::Session,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .expect("create");

    // Simulate successful extraction.
    memory::set_extraction_status(&db, &id, ExtractionStatus::Complete)
        .await
        .expect("set complete");

    // A concurrent/racing call tries to write a failure — must be a no-op.
    memory::set_extraction_status(&db, &id, ExtractionStatus::BothFailed)
        .await
        .expect("set both_failed after complete");

    let mem = memory::get(&db, &id).await.expect("get");
    assert_eq!(
        mem.extraction_status,
        Some(ExtractionStatus::Complete),
        "complete must not be overwritten by a failure status"
    );
}

/// Once skipped, set_extraction_status must NOT overwrite with any other status.
#[tokio::test]
async fn set_extraction_status_does_not_overwrite_skipped() {
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Classified project information about internal roadmap planning".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .expect("create");

    memory::set_extraction_status(&db, &id, ExtractionStatus::Skipped)
        .await
        .expect("set skipped");

    memory::set_extraction_status(&db, &id, ExtractionStatus::Complete)
        .await
        .expect("set complete after skipped");

    let mem = memory::get(&db, &id).await.expect("get");
    assert_eq!(
        mem.extraction_status,
        Some(ExtractionStatus::Skipped),
        "skipped must not be overwritten"
    );
}

/// A failed status CAN transition to complete (retry path).
#[tokio::test]
async fn set_extraction_status_failed_to_complete_retry() {
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Completed the infrastructure migration to new datacenter region".into(),
            MemoryType::Episodic,
            MemorySubtype::Session,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .expect("create");

    // First attempt fails.
    memory::set_extraction_status(&db, &id, ExtractionStatus::BothFailed)
        .await
        .expect("set both_failed");

    // Retry succeeds.
    memory::set_extraction_status(&db, &id, ExtractionStatus::Complete)
        .await
        .expect("set complete on retry");

    let mem = memory::get(&db, &id).await.expect("get");
    assert_eq!(
        mem.extraction_status,
        Some(ExtractionStatus::Complete),
        "retry should transition failed → complete"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// ExtractionStatus enum unit tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn extraction_status_as_str_roundtrip_all_variants() {
    let all = [
        ExtractionStatus::Pending,
        ExtractionStatus::Complete,
        ExtractionStatus::AnchorFailed,
        ExtractionStatus::RelationFailed,
        ExtractionStatus::BothFailed,
        ExtractionStatus::SidecarUnavailable,
        ExtractionStatus::Skipped,
    ];
    for variant in &all {
        let s = variant.as_str();
        let parsed: ExtractionStatus = s.parse().expect("FromStr should parse as_str");
        assert_eq!(*variant, parsed, "roundtrip failed for {variant:?}");
    }
}

#[test]
fn extraction_status_serde_roundtrip() {
    let all = [
        ExtractionStatus::Pending,
        ExtractionStatus::Complete,
        ExtractionStatus::AnchorFailed,
        ExtractionStatus::RelationFailed,
        ExtractionStatus::BothFailed,
        ExtractionStatus::SidecarUnavailable,
        ExtractionStatus::Skipped,
    ];
    for variant in &all {
        let json = serde_json::to_string(variant).expect("serialize");
        let back: ExtractionStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*variant, back);
    }
}

#[test]
fn extraction_status_is_retryable() {
    assert!(ExtractionStatus::AnchorFailed.is_retryable());
    assert!(ExtractionStatus::RelationFailed.is_retryable());
    assert!(ExtractionStatus::BothFailed.is_retryable());
    assert!(ExtractionStatus::SidecarUnavailable.is_retryable());
    assert!(!ExtractionStatus::Pending.is_retryable());
    assert!(!ExtractionStatus::Complete.is_retryable());
    assert!(!ExtractionStatus::Skipped.is_retryable());
}

#[test]
fn extraction_status_display_matches_as_str() {
    let all = [
        ExtractionStatus::Pending,
        ExtractionStatus::Complete,
        ExtractionStatus::AnchorFailed,
        ExtractionStatus::RelationFailed,
        ExtractionStatus::BothFailed,
        ExtractionStatus::SidecarUnavailable,
        ExtractionStatus::Skipped,
    ];
    for v in all {
        assert_eq!(v.as_str(), format!("{v}"));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ExtractionBacklog alert
// ═══════════════════════════════════════════════════════════════════════════

/// When count of failed extractions exceeds threshold, ExtractionBacklog alert fires.
#[tokio::test]
async fn extraction_backlog_alert_fires_when_threshold_exceeded() {
    let (db, _dir) = test_db().await;

    // Create 3 memories with failed extraction status.
    for i in 0..3 {
        let id = memory::create(
            &db,
            memory::MemoryDraft::new(
                format!("Team discussed plans for project migration phase {}", i + 1),
                MemoryType::Episodic,
                MemorySubtype::Meeting,
                Some(Channel::Cli),
                Some(Modality::Text),
            ),
        )
        .await
        .expect("create");

        memory::set_extraction_status(&db, &id, ExtractionStatus::BothFailed)
            .await
            .expect("set both_failed");
    }

    // Threshold of 2 — should trigger alert since count (3) > threshold (2).
    let config = AlertConfig {
        overdue_action_item_days: 1,
        stale_contact_days: 14,
        expiring_memory_hours: 24,
        extraction_backlog_threshold: 2,
    };

    let alert_list = alerts::check_all(&db, &config).await.expect("check_all");
    assert!(
        alert_list
            .iter()
            .any(|a| a.alert_type == AlertType::ExtractionBacklog),
        "ExtractionBacklog alert must fire when count exceeds threshold"
    );
}

/// When count is at or below threshold, no ExtractionBacklog alert fires.
#[tokio::test]
async fn extraction_backlog_alert_silent_when_below_threshold() {
    let (db, _dir) = test_db().await;

    // Create 2 memories with failed extraction status.
    for i in 0..2 {
        let id = memory::create(
            &db,
            memory::MemoryDraft::new(
                format!("Reviewed deployment pipeline configuration for service {}", i + 1),
                MemoryType::Episodic,
                MemorySubtype::Meeting,
                Some(Channel::Cli),
                Some(Modality::Text),
            ),
        )
        .await
        .expect("create");

        memory::set_extraction_status(&db, &id, ExtractionStatus::AnchorFailed)
            .await
            .expect("set anchor_failed");
    }

    // Threshold of 50 — count (2) is well below.
    let config = AlertConfig {
        overdue_action_item_days: 1,
        stale_contact_days: 14,
        expiring_memory_hours: 24,
        extraction_backlog_threshold: 50,
    };

    let alert_list = alerts::check_all(&db, &config).await.expect("check_all");
    assert!(
        !alert_list
            .iter()
            .any(|a| a.alert_type == AlertType::ExtractionBacklog),
        "ExtractionBacklog alert must NOT fire when count is at/below threshold"
    );
}

/// 'pending' and 'complete' and 'skipped' statuses do NOT count toward the backlog.
#[tokio::test]
async fn extraction_backlog_excludes_non_failed_statuses() {
    let (db, _dir) = test_db().await;

    // Create memories with non-failure statuses — none should count.
    let statuses_to_test = [
        ExtractionStatus::Pending,
        ExtractionStatus::Complete,
        ExtractionStatus::Skipped,
    ];

    for (i, status) in statuses_to_test.iter().enumerate() {
        let id = memory::create(
            &db,
            memory::MemoryDraft::new(
                format!("Infrastructure health check completed for region cluster {}", i + 1),
                MemoryType::Semantic,
                MemorySubtype::Fact,
                Some(Channel::Cli),
                Some(Modality::Text),
            ),
        )
        .await
        .expect("create");

        memory::set_extraction_status(&db, &id, *status)
            .await
            .expect("set status");
    }

    // Threshold of 1 — even with 3 memories, none are failures.
    let config = AlertConfig {
        overdue_action_item_days: 1,
        stale_contact_days: 14,
        expiring_memory_hours: 24,
        extraction_backlog_threshold: 1,
    };

    let alert_list = alerts::check_all(&db, &config).await.expect("check_all");
    assert!(
        !alert_list
            .iter()
            .any(|a| a.alert_type == AlertType::ExtractionBacklog),
        "pending/complete/skipped must not count toward extraction backlog"
    );
}
