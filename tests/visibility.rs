//! Visibility enforcement tests.
//!
//! Verifies that private and sensitive memories are correctly filtered
//! across all query paths: recall, status, alerts, entity_get,
//! spreading activation, and consolidation candidates.

mod common;
use common::test_db;

use chrono::{Duration, Utc};
use smriti::config::AlertConfig;
use smriti::db::{alerts, consolidation, entity, graph, memory, search};
use smriti::tools::remember;
use smriti::types::*;
use std::sync::Arc;
use smriti::shutdown::TaskRegistry;

// ═══════════════════════════════════════════════════════════════════════════
// RECALL VISIBILITY
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn private_memory_visible_within_project() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db,
        memory::MemoryDraft::new(
            "Private insight for proj-a".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .project("proj-a".into())
        .visibility(Visibility::Private),
    )
    .await
    .unwrap();

    // Recall with project filter — private should appear
    let results = search::recall(
        &db, "", None, None, None, Some("proj-a"), None, None, None, None, None, None, 10,
        &search::AuthorFilter::None, 0.0,
        &Default::default(), &Default::default(), RerankerPolicy::default(), None, None, &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 1, "private memory should be visible within its project");
}

#[tokio::test]
async fn private_memory_hidden_without_project_filter() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db,
        memory::MemoryDraft::new(
            "Private insight for proj-a".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .project("proj-a".into())
        .visibility(Visibility::Private),
    )
    .await
    .unwrap();

    // Recall without project filter (cross-project) — private should NOT appear
    let results = search::recall(
        &db, "", None, None, None, None, None, None, None, None, None, None, 10,
        &search::AuthorFilter::None, 0.0,
        &Default::default(), &Default::default(), RerankerPolicy::default(), None, None, &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 0, "private memory should be hidden in cross-project recall");
}

#[tokio::test]
async fn sensitive_memory_hidden_with_project_filter() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db,
        memory::MemoryDraft::new(
            "Sensitive personal note".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .project("proj-a".into())
        .visibility(Visibility::Sensitive),
    )
    .await
    .unwrap();

    // Recall with matching project filter — sensitive should still NOT appear
    let results = search::recall(
        &db, "", None, None, None, Some("proj-a"), None, None, None, None, None, None, 10,
        &search::AuthorFilter::None, 0.0,
        &Default::default(), &Default::default(), RerankerPolicy::default(), None, None, &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 0, "sensitive memory should never appear in recall");
}

#[tokio::test]
async fn sensitive_memory_hidden_without_project_filter() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db,
        memory::MemoryDraft::new(
            "Sensitive personal note".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .visibility(Visibility::Sensitive),
    )
    .await
    .unwrap();

    let results = search::recall(
        &db, "", None, None, None, None, None, None, None, None, None, None, 10,
        &search::AuthorFilter::None, 0.0,
        &Default::default(), &Default::default(), RerankerPolicy::default(), None, None, &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 0, "sensitive memory should never appear in cross-project recall");
}

#[tokio::test]
async fn standard_memory_visible_everywhere() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db,
        memory::MemoryDraft::new(
            "Standard memory visible everywhere".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .project("proj-a".into())
        .visibility(Visibility::Standard),
    )
    .await
    .unwrap();

    // With project filter
    let with_project = search::recall(
        &db, "", None, None, None, Some("proj-a"), None, None, None, None, None, None, 10,
        &search::AuthorFilter::None, 0.0,
        &Default::default(), &Default::default(), RerankerPolicy::default(), None, None, &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(with_project.len(), 1, "standard should appear with project filter");

    // Without project filter
    let without_project = search::recall(
        &db, "", None, None, None, None, None, None, None, None, None, None, 10,
        &search::AuthorFilter::None, 0.0,
        &Default::default(), &Default::default(), RerankerPolicy::default(), None, None, &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(without_project.len(), 1, "standard should appear without project filter");
}

// ═══════════════════════════════════════════════════════════════════════════
// STATUS VISIBILITY
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn status_hides_private_in_cross_project() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db,
        memory::MemoryDraft::new(
            "Private action item".into(),
            MemoryType::Prospective,
            MemorySubtype::ActionItem,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .project("proj-a".into())
        .visibility(Visibility::Private),
    )
    .await
    .unwrap();

    // Cross-project status should hide private
    let status = search::status_query(&db, None, None).await.unwrap();
    assert!(status.open_action_items.is_empty(), "private action item should not appear in cross-project status");

    // Project-scoped status should show it
    let status = search::status_query(&db, Some("proj-a"), None).await.unwrap();
    assert_eq!(status.open_action_items.len(), 1, "private action item should appear in project-scoped status");
}

#[tokio::test]
async fn status_hides_sensitive_always() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db,
        memory::MemoryDraft::new(
            "Sensitive decision".into(),
            MemoryType::Semantic,
            MemorySubtype::Decision,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .project("proj-a".into())
        .importance(Importance::new(4).unwrap())
        .visibility(Visibility::Sensitive),
    )
    .await
    .unwrap();

    let status = search::status_query(&db, Some("proj-a"), None).await.unwrap();
    assert!(status.recent_decisions.is_empty(), "sensitive decision should not appear in status");
}

// ═══════════════════════════════════════════════════════════════════════════
// ALERT VISIBILITY
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn sensitive_action_item_not_in_alerts() {
    let (db, _dir) = test_db().await;

    // Create a sensitive overdue action item
    memory::create(
        &db,
        memory::MemoryDraft::new(
            "Sensitive overdue task".into(),
            MemoryType::Prospective,
            MemorySubtype::ActionItem,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .visibility(Visibility::Sensitive)
        .event_at(Utc::now() - Duration::days(3)),
    )
    .await
    .unwrap();

    // Also create a private overdue action item
    memory::create(
        &db,
        memory::MemoryDraft::new(
            "Private overdue task".into(),
            MemoryType::Prospective,
            MemorySubtype::ActionItem,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .project("proj-a".into())
        .visibility(Visibility::Private)
        .event_at(Utc::now() - Duration::days(3)),
    )
    .await
    .unwrap();

    let config = AlertConfig {
        overdue_action_item_days: 1,
        stale_contact_days: 14,
        expiring_memory_hours: 24,
        extraction_backlog_threshold: 50,
    };

    let all_alerts = alerts::check_all(&db, &config).await.unwrap();
    assert!(
        !all_alerts.iter().any(|a| a.alert_type == AlertType::OverdueActionItem),
        "sensitive and private action items should NOT appear in alerts (cross-context = standard only)"
    );
}

#[tokio::test]
async fn sensitive_expiring_memory_not_in_alerts() {
    let (db, _dir) = test_db().await;

    // Create a sensitive memory about to expire
    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Sensitive expiring note".into(),
            MemoryType::Working,
            MemorySubtype::Scratch,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .visibility(Visibility::Sensitive)
        .importance(Importance::new(1).unwrap()),
    )
    .await
    .unwrap();

    // Set expires_at to 6 hours from now
    db.query("UPDATE type::record('memory', $id) SET expires_at = time::now() + 6h")
        .bind(("id", id.as_str().to_string()))
        .await.unwrap();

    let config = AlertConfig {
        overdue_action_item_days: 1,
        stale_contact_days: 14,
        expiring_memory_hours: 24,
        extraction_backlog_threshold: 50,
    };

    let all_alerts = alerts::check_all(&db, &config).await.unwrap();
    assert!(
        !all_alerts.iter().any(|a| a.alert_type == AlertType::ExpiringMemory),
        "sensitive expiring memory should NOT appear in alerts"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// ENTITY_GET VISIBILITY
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn entity_get_hides_sensitive_and_private_memories() {
    let (db, _dir) = test_db().await;

    // Create entity
    let entity_id = entity::upsert(
        &db, "Alice", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    ).await.unwrap();

    // Create a standard memory linked to Alice
    let standard_id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Standard meeting with Alice".into(),
            MemoryType::Episodic,
            MemorySubtype::Meeting,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .people(vec!["Alice".into()])
        .visibility(Visibility::Standard),
    )
    .await
    .unwrap();
    graph::link_entity(&db, &standard_id, "Alice").await.unwrap();

    // Create a sensitive memory linked to Alice
    let sensitive_id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Sensitive note about Alice".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .people(vec!["Alice".into()])
        .visibility(Visibility::Sensitive),
    )
    .await
    .unwrap();
    graph::link_entity(&db, &sensitive_id, "Alice").await.unwrap();

    // Create a private memory linked to Alice
    let private_id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Private note about Alice".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .project("proj-a".into())
        .people(vec!["Alice".into()])
        .visibility(Visibility::Private),
    )
    .await
    .unwrap();
    graph::link_entity(&db, &private_id, "Alice").await.unwrap();

    let full = entity::get_full(&db, &entity_id).await.unwrap();

    // Only standard memory should appear in recent_memories
    assert_eq!(
        full.recent_memories.len(), 1,
        "entity_get should only show standard memories, got {}",
        full.recent_memories.len()
    );
    assert_eq!(full.recent_memories[0].id, standard_id);
}

// ═══════════════════════════════════════════════════════════════════════════
// SPREADING ACTIVATION VISIBILITY
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn spreading_activation_hides_private_and_sensitive() {
    let (db, _dir) = test_db().await;

    // Create a standard memory (will be found by BM25)
    let primary = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Primary standard fact about architecture".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .project("proj-a".into())
        .importance(Importance::new(5).unwrap())
        .visibility(Visibility::Standard),
    )
    .await
    .unwrap();

    // Create a private neighbor linked via extends
    let private_neighbor = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Private detail about architecture".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .project("proj-b".into())
        .importance(Importance::new(4).unwrap())
        .visibility(Visibility::Private),
    )
    .await
    .unwrap();
    graph::link_memories(&db, &private_neighbor, &primary, RelationType::Extends).await.unwrap();

    // Create a sensitive neighbor linked via relates
    let sensitive_neighbor = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Sensitive insight on architecture".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        )
        .importance(Importance::new(4).unwrap())
        .visibility(Visibility::Sensitive),
    )
    .await
    .unwrap();
    graph::link_memories(&db, &sensitive_neighbor, &primary, RelationType::Relates).await.unwrap();

    // Recall without project filter — spreading activation should NOT surface private/sensitive
    let results = search::recall(
        &db, "", None, None, None, None, None, None, None, None, None, None, 25,
        &search::AuthorFilter::None, 0.0,
        &Default::default(), &Default::default(), RerankerPolicy::default(), None, None, &Default::default(),
    )
    .await
    .unwrap();

    let ids: Vec<&str> = results.iter().map(|m| m.id.as_str()).collect();
    assert!(ids.contains(&primary.as_str()), "primary should be in results");
    assert!(!ids.contains(&private_neighbor.as_str()), "private neighbor should NOT be surfaced by spreading activation");
    assert!(!ids.contains(&sensitive_neighbor.as_str()), "sensitive neighbor should NOT be surfaced by spreading activation");
}

// ═══════════════════════════════════════════════════════════════════════════
// CONSOLIDATION CANDIDATE VISIBILITY
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn consolidation_candidates_hide_sensitive() {
    let (db, _dir) = test_db().await;

    // Create 3 sensitive memories in same project+subtype
    for i in 0..3 {
        memory::create(
            &db,
            memory::MemoryDraft::new(
                format!("Sensitive fact {i}"),
                MemoryType::Semantic,
                MemorySubtype::Fact,
                Some(Channel::Cli), Some(Modality::Text),
            )
            .project("proj-a".into())
            .visibility(Visibility::Sensitive),
        )
        .await
        .unwrap();
    }

    let candidates = consolidation::find_candidates(&db, 10, None, None).await.unwrap();
    assert!(
        candidates.is_empty(),
        "sensitive memories should not appear as consolidation candidates"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// PATH SAFETY (EXPORT/IMPORT)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn path_safety_rejects_traversal() {
    use smriti::db::path_safety;

    assert!(path_safety::safe_filename("../etc/passwd").is_err());
    assert!(path_safety::safe_filename("/etc/shadow").is_err());
    assert!(path_safety::safe_filename("foo/../bar").is_err());
    assert!(path_safety::safe_filename(".hidden").is_err());
    assert!(path_safety::safe_filename("").is_err());
    assert!(path_safety::safe_filename("valid_export.jsonl").is_ok());
}

// ═══════════════════════════════════════════════════════════════════════════
// SUPERSESSION ATOMICITY
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn supersede_already_archived_returns_false() {
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "will be archived".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        ),
    )
    .await
    .unwrap();

    memory::archive(&db, &id, InvalidationReason::Forgotten).await.unwrap();

    // Supersede on already-archived should return false (not active)
    let result = memory::supersede(&db, &id, Utc::now()).await.unwrap();
    assert!(!result, "supersede of archived memory should return false");

    // Status should still be archived, not superseded
    let mut raw = db.query("SELECT status FROM type::record('memory', $id)")
        .bind(("id", id.as_str().to_string()))
        .await.unwrap();
    let row: Option<serde_json::Value> = raw.take(0).unwrap();
    let status = row.and_then(|v| v.get("status").and_then(|s| s.as_str()).map(String::from)).unwrap_or_default();
    assert_eq!(status, "archived");
}

#[tokio::test]
async fn supersede_already_superseded_returns_false() {
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "will be superseded".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        ),
    )
    .await
    .unwrap();

    let first = memory::supersede(&db, &id, Utc::now()).await.unwrap();
    assert!(first, "first supersede should succeed");

    let second = memory::supersede(&db, &id, Utc::now()).await.unwrap();
    assert!(!second, "second supersede should return false (already superseded)");
}

// ═══════════════════════════════════════════════════════════════════════════
// SM-2 REVIEW INTERVAL CAP
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn sm2_review_interval_capped_at_365() {
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "long-reviewed memory for SM2 test".into(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli), Some(Modality::Text),
        ),
    )
    .await
    .unwrap();

    // Manually set very high SM-2 values that would overflow without cap
    db.query("UPDATE type::record('memory', $id) SET ease_factor = 4.0, review_interval = 200")
        .bind(("id", id.as_str().to_string()))
        .await.unwrap();

    // Trigger a recall which fires SM-2 update in background
    let _ = search::recall(
        &db, "", None, None, None, None, None, None, None, None, None, None, 10,
        &search::AuthorFilter::None, 0.0,
        &Default::default(), &Default::default(), RerankerPolicy::default(), None, None, &Default::default(),
    )
    .await
    .unwrap();

    // Give the background task time to complete
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Check that review_interval is capped
    let mut result = db.query("SELECT review_interval FROM type::record('memory', $id)")
        .bind(("id", id.as_str().to_string()))
        .await.unwrap();
    let row: Option<serde_json::Value> = result.take(0).unwrap();
    let ri = row.and_then(|v| v.get("review_interval").and_then(|r| r.as_i64())).unwrap_or(0);

    assert!(ri <= 365, "review_interval should be capped at 365, got {ri}");
}

// ═══════════════════════════════════════════════════════════════════════════
// CONTENT SIZE LIMIT
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn content_over_500kb_rejected() {
    use smriti::db::validation;

    let huge = "x".repeat(500 * 1024 + 1);
    let result = validation::validate_content(Some(&huge));
    assert!(result.is_err(), "content over 500KB should be rejected");

    let ok = "x".repeat(500 * 1024);
    let result = validation::validate_content(Some(&ok));
    assert!(result.is_ok(), "content at exactly 500KB should be accepted");

    let result = validation::validate_content(None);
    assert!(result.is_ok(), "None content should be accepted");
}

// ═══════════════════════════════════════════════════════════════════════════
// CHANNEL-BASED VISIBILITY DEFAULTS  (ADR-019)
// ═══════════════════════════════════════════════════════════════════════════

/// Helper: write a memory via the full remember tool path and return its
/// stored visibility string as read from the DB.
async fn stored_visibility(
    db: &smriti::db::client::SurrealClient,
    id: &str,
) -> String {
    let mut result = db
        .query("SELECT visibility FROM type::record('memory', $id)")
        .bind(("id", id.to_string()))
        .await
        .unwrap();
    let row: Option<serde_json::Value> = result.take(0).unwrap();
    row.and_then(|v| v.get("visibility").and_then(|s| s.as_str()).map(String::from))
        .unwrap_or_default()
}

#[tokio::test]
async fn channel_slack_no_explicit_visibility_defaults_to_private() {
    let (db, _dir) = test_db().await;

    let out = remember::execute(
        &db, None, None, Arc::new(TaskRegistry::new()),
        remember::RememberInput {
            summary: "Slack message about project status".to_string(),
            memory_type: MemoryType::Episodic,
            subtype: MemorySubtype::Conversation,
            channel: Some(Channel::Slack),
            modality: Some(Modality::Text),
            content: None,
            project: Some("proj-a".to_string()),
            domain: None,
            people: None,
            importance: None,
            visibility: None,
            event_at: None,
            tags: None,
            updates_id: None,
            source_agent: None,
            is_static: None,
        },
    ).await.unwrap();

    assert_eq!(
        stored_visibility(&db, &out.id).await,
        "private",
        "channel=Slack without explicit visibility must default to private"
    );
}

#[tokio::test]
async fn channel_slack_explicit_standard_visibility_wins() {
    let (db, _dir) = test_db().await;

    let out = remember::execute(
        &db, None, None, Arc::new(TaskRegistry::new()),
        remember::RememberInput {
            summary: "Slack message with explicit standard visibility".to_string(),
            memory_type: MemoryType::Episodic,
            subtype: MemorySubtype::Conversation,
            channel: Some(Channel::Slack),
            modality: Some(Modality::Text),
            content: None,
            project: None,
            domain: None,
            people: None,
            importance: None,
            visibility: Some(Visibility::Standard),
            event_at: None,
            tags: None,
            updates_id: None,
            source_agent: None,
            is_static: None,
        },
    ).await.unwrap();

    assert_eq!(
        stored_visibility(&db, &out.id).await,
        "standard",
        "explicit visibility=Standard must override the Slack channel default"
    );
}

#[tokio::test]
async fn channel_discord_no_explicit_visibility_defaults_to_private() {
    let (db, _dir) = test_db().await;

    let out = remember::execute(
        &db, None, None, Arc::new(TaskRegistry::new()),
        remember::RememberInput {
            summary: "Discord channel message about deployment".to_string(),
            memory_type: MemoryType::Episodic,
            subtype: MemorySubtype::Conversation,
            channel: Some(Channel::Discord),
            modality: Some(Modality::Text),
            content: None,
            project: None,
            domain: None,
            people: None,
            importance: None,
            visibility: None,
            event_at: None,
            tags: None,
            updates_id: None,
            source_agent: None,
            is_static: None,
        },
    ).await.unwrap();

    assert_eq!(
        stored_visibility(&db, &out.id).await,
        "private",
        "channel=Discord without explicit visibility must default to private"
    );
}

#[tokio::test]
async fn channel_voice_app_no_explicit_visibility_defaults_to_private() {
    let (db, _dir) = test_db().await;

    let out = remember::execute(
        &db, None, None, Arc::new(TaskRegistry::new()),
        remember::RememberInput {
            summary: "Voice memo transcript from morning standup".to_string(),
            memory_type: MemoryType::Episodic,
            subtype: MemorySubtype::Meeting,
            channel: Some(Channel::VoiceApp),
            modality: Some(Modality::Transcript),
            content: None,
            project: None,
            domain: None,
            people: None,
            importance: None,
            visibility: None,
            event_at: None,
            tags: None,
            updates_id: None,
            source_agent: None,
            is_static: None,
        },
    ).await.unwrap();

    assert_eq!(
        stored_visibility(&db, &out.id).await,
        "private",
        "channel=VoiceApp without explicit visibility must default to private"
    );
}

#[tokio::test]
async fn channel_sms_no_explicit_visibility_defaults_to_private() {
    let (db, _dir) = test_db().await;

    let out = remember::execute(
        &db, None, None, Arc::new(TaskRegistry::new()),
        remember::RememberInput {
            summary: "SMS message about meeting rescheduled to Friday".to_string(),
            memory_type: MemoryType::Episodic,
            subtype: MemorySubtype::Conversation,
            channel: Some(Channel::Sms),
            modality: Some(Modality::Text),
            content: None,
            project: None,
            domain: None,
            people: None,
            importance: None,
            visibility: None,
            event_at: None,
            tags: None,
            updates_id: None,
            source_agent: None,
            is_static: None,
        },
    ).await.unwrap();

    assert_eq!(
        stored_visibility(&db, &out.id).await,
        "private",
        "channel=Sms without explicit visibility must default to private"
    );
}

#[tokio::test]
async fn channel_cli_no_explicit_visibility_stays_standard() {
    let (db, _dir) = test_db().await;

    let out = remember::execute(
        &db, None, None, Arc::new(TaskRegistry::new()),
        remember::RememberInput {
            summary: "CLI entry about project architecture decision".to_string(),
            memory_type: MemoryType::Semantic,
            subtype: MemorySubtype::Decision,
            channel: Some(Channel::Cli),
            modality: Some(Modality::Text),
            content: None,
            project: None,
            domain: None,
            people: None,
            importance: Some(4),
            visibility: None,
            event_at: None,
            tags: None,
            updates_id: None,
            source_agent: None,
            is_static: None,
        },
    ).await.unwrap();

    assert_eq!(
        stored_visibility(&db, &out.id).await,
        "standard",
        "channel=Cli without explicit visibility must remain standard"
    );
}

#[tokio::test]
async fn no_channel_no_explicit_visibility_stays_standard() {
    let (db, _dir) = test_db().await;

    let out = remember::execute(
        &db, None, None, Arc::new(TaskRegistry::new()),
        remember::RememberInput {
            summary: "Manual entry with no channel specified stays standard".to_string(),
            memory_type: MemoryType::Semantic,
            subtype: MemorySubtype::Fact,
            channel: None,
            modality: None,
            content: None,
            project: None,
            domain: None,
            people: None,
            importance: None,
            visibility: None,
            event_at: None,
            tags: None,
            updates_id: None,
            source_agent: None,
            is_static: None,
        },
    ).await.unwrap();

    assert_eq!(
        stored_visibility(&db, &out.id).await,
        "standard",
        "no channel without explicit visibility must remain standard"
    );
}
