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
use smriti::types::*;

// ═══════════════════════════════════════════════════════════════════════════
// RECALL VISIBILITY
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn private_memory_visible_within_project() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db, "Private insight for proj-a".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::default(),
        Visibility::Private, None, vec![], None, None, None,
    ).await.unwrap();

    // Recall with project filter — private should appear
    let results = search::recall(
        &db, "", None, None, Some("proj-a"), None, None, None, None, 10,
    ).await.unwrap();
    assert_eq!(results.len(), 1, "private memory should be visible within its project");
}

#[tokio::test]
async fn private_memory_hidden_without_project_filter() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db, "Private insight for proj-a".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::default(),
        Visibility::Private, None, vec![], None, None, None,
    ).await.unwrap();

    // Recall without project filter (cross-project) — private should NOT appear
    let results = search::recall(
        &db, "", None, None, None, None, None, None, None, 10,
    ).await.unwrap();
    assert_eq!(results.len(), 0, "private memory should be hidden in cross-project recall");
}

#[tokio::test]
async fn sensitive_memory_hidden_with_project_filter() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db, "Sensitive personal note".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::default(),
        Visibility::Sensitive, None, vec![], None, None, None,
    ).await.unwrap();

    // Recall with matching project filter — sensitive should still NOT appear
    let results = search::recall(
        &db, "", None, None, Some("proj-a"), None, None, None, None, 10,
    ).await.unwrap();
    assert_eq!(results.len(), 0, "sensitive memory should never appear in recall");
}

#[tokio::test]
async fn sensitive_memory_hidden_without_project_filter() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db, "Sensitive personal note".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::Sensitive, None, vec![], None, None, None,
    ).await.unwrap();

    let results = search::recall(
        &db, "", None, None, None, None, None, None, None, 10,
    ).await.unwrap();
    assert_eq!(results.len(), 0, "sensitive memory should never appear in cross-project recall");
}

#[tokio::test]
async fn standard_memory_visible_everywhere() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db, "Standard memory visible everywhere".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::default(),
        Visibility::Standard, None, vec![], None, None, None,
    ).await.unwrap();

    // With project filter
    let with_project = search::recall(
        &db, "", None, None, Some("proj-a"), None, None, None, None, 10,
    ).await.unwrap();
    assert_eq!(with_project.len(), 1, "standard should appear with project filter");

    // Without project filter
    let without_project = search::recall(
        &db, "", None, None, None, None, None, None, None, 10,
    ).await.unwrap();
    assert_eq!(without_project.len(), 1, "standard should appear without project filter");
}

// ═══════════════════════════════════════════════════════════════════════════
// STATUS VISIBILITY
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn status_hides_private_in_cross_project() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db, "Private action item".into(), None,
        MemoryType::Prospective, MemorySubtype::ActionItem, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::default(),
        Visibility::Private, None, vec![], None, None, None,
    ).await.unwrap();

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
        &db, "Sensitive decision".into(), None,
        MemoryType::Semantic, MemorySubtype::Decision, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::default(),
        Visibility::Sensitive, None, vec![], None, None, None,
    ).await.unwrap();

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
        &db, "Sensitive overdue task".into(), None,
        MemoryType::Prospective, MemorySubtype::ActionItem, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::Sensitive,
        Some(Utc::now() - Duration::days(3)),
        vec![], None, None, None,
    ).await.unwrap();

    // Also create a private overdue action item
    memory::create(
        &db, "Private overdue task".into(), None,
        MemoryType::Prospective, MemorySubtype::ActionItem, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::default(),
        Visibility::Private,
        Some(Utc::now() - Duration::days(3)),
        vec![], None, None, None,
    ).await.unwrap();

    let config = AlertConfig {
        overdue_action_item_days: 1,
        stale_contact_days: 14,
        expiring_memory_hours: 24,
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
        &db, "Sensitive expiring note".into(), None,
        MemoryType::Working, MemorySubtype::Scratch, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::Sensitive, None, vec![], None, None, None,
    ).await.unwrap();

    // Set expires_at to 6 hours from now
    db.query("UPDATE type::record('memory', $id) SET expires_at = time::now() + 6h")
        .bind(("id", id.as_str().to_string()))
        .await.unwrap();

    let config = AlertConfig {
        overdue_action_item_days: 1,
        stale_contact_days: 14,
        expiring_memory_hours: 24,
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
        &db, "Alice", "person", vec![], serde_json::json!({}), None,
    ).await.unwrap();

    // Create a standard memory linked to Alice
    let standard_id = memory::create(
        &db, "Standard meeting with Alice".into(), None,
        MemoryType::Episodic, MemorySubtype::Meeting, Source::Manual,
        None, vec![], vec!["Alice".into()], Importance::default(),
        Visibility::Standard, None, vec![], None, None, None,
    ).await.unwrap();
    graph::link_entity(&db, &standard_id, "Alice").await.unwrap();

    // Create a sensitive memory linked to Alice
    let sensitive_id = memory::create(
        &db, "Sensitive note about Alice".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec!["Alice".into()], Importance::default(),
        Visibility::Sensitive, None, vec![], None, None, None,
    ).await.unwrap();
    graph::link_entity(&db, &sensitive_id, "Alice").await.unwrap();

    // Create a private memory linked to Alice
    let private_id = memory::create(
        &db, "Private note about Alice".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        Some("proj-a".into()), vec![], vec!["Alice".into()], Importance::default(),
        Visibility::Private, None, vec![], None, None, None,
    ).await.unwrap();
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
        &db, "Primary standard fact about architecture".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::new(5).unwrap(),
        Visibility::Standard, None, vec![], None, None, None,
    ).await.unwrap();

    // Create a private neighbor linked via extends
    let private_neighbor = memory::create(
        &db, "Private detail about architecture".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        Some("proj-b".into()), vec![], vec![], Importance::new(4).unwrap(),
        Visibility::Private, None, vec![], None, None, None,
    ).await.unwrap();
    graph::link_memories(&db, &private_neighbor, &primary, RelationType::Extends).await.unwrap();

    // Create a sensitive neighbor linked via relates
    let sensitive_neighbor = memory::create(
        &db, "Sensitive insight on architecture".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::new(4).unwrap(),
        Visibility::Sensitive, None, vec![], None, None, None,
    ).await.unwrap();
    graph::link_memories(&db, &sensitive_neighbor, &primary, RelationType::Relates).await.unwrap();

    // Recall without project filter — spreading activation should NOT surface private/sensitive
    let results = search::recall(
        &db, "", None, None, None, None, None, None, None, 25,
    ).await.unwrap();

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
            &db, format!("Sensitive fact {i}"), None,
            MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
            Some("proj-a".into()), vec![], vec![], Importance::default(),
            Visibility::Sensitive, None, vec![], None, None, None,
        ).await.unwrap();
    }

    let candidates = consolidation::find_candidates(&db, 10).await.unwrap();
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
        &db, "will be archived".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    memory::archive(&db, &id).await.unwrap();

    // Supersede on already-archived should return false (not active)
    let result = memory::supersede(&db, &id).await.unwrap();
    assert!(!result, "supersede of archived memory should return false");

    // Status should still be archived, not superseded
    let mem = memory::get(&db, &id).await.unwrap();
    assert_eq!(mem.status, MemoryStatus::Archived);
}

#[tokio::test]
async fn supersede_already_superseded_returns_false() {
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db, "will be superseded".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    let first = memory::supersede(&db, &id).await.unwrap();
    assert!(first, "first supersede should succeed");

    let second = memory::supersede(&db, &id).await.unwrap();
    assert!(!second, "second supersede should return false (already superseded)");
}

// ═══════════════════════════════════════════════════════════════════════════
// SM-2 REVIEW INTERVAL CAP
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn sm2_review_interval_capped_at_365() {
    // Test the compute_sm2 logic indirectly: create a memory with high
    // ease_factor and review_interval, then verify the stored interval
    // doesn't exceed 365 after a recall triggers SM-2 update.
    let (db, _dir) = test_db().await;

    let id = memory::create(
        &db, "long-reviewed memory for SM2 test".into(), None,
        MemoryType::Semantic, MemorySubtype::Fact, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    // Manually set very high SM-2 values that would overflow without cap
    db.query("UPDATE type::record('memory', $id) SET ease_factor = 4.0, review_interval = 200")
        .bind(("id", id.as_str().to_string()))
        .await.unwrap();

    // Trigger a recall which fires SM-2 update in background
    let _ = search::recall(&db, "", None, None, None, None, None, None, None, 10).await.unwrap();

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
