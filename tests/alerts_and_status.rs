mod common;
use common::test_db;

use chrono::{Duration, Utc};
use kb_server::db::{alerts, graph, memory, search};
use kb_server::config::AlertConfig;
use kb_server::types::*;

// ═══════════════════════════════════════════════════════════════════════════
// STATUS QUERY
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn status_returns_action_items_and_decisions() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db, "Ship feature X by Friday".into(), None,
        MemoryType::Prospective, MemorySubtype::ActionItem, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::new(4).unwrap(), Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    memory::create(
        &db, "Use Rust for backend".into(), None,
        MemoryType::Semantic, MemorySubtype::Decision, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::new(5).unwrap(), Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    memory::create(
        &db, "Should we support multi-tenant?".into(), None,
        MemoryType::Working, MemorySubtype::OpenQuestion, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::default(), Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    let status = search::status_query(&db, Some("proj-a"), None).await.unwrap();
    assert_eq!(status.open_action_items.len(), 1);
    assert_eq!(status.open_action_items[0].summary, "Ship feature X by Friday");
    assert_eq!(status.recent_decisions.len(), 1);
    assert_eq!(status.recent_decisions[0].summary, "Use Rust for backend");
    assert_eq!(status.open_questions.len(), 1);
    assert_eq!(status.total_memories, 3);
    assert_eq!(status.active_memories, 3);
}

#[tokio::test]
async fn status_filters_by_project() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db, "proj A item".into(), None,
        MemoryType::Prospective, MemorySubtype::ActionItem, Source::Manual,
        Some("proj-a".into()), vec![], vec![], Importance::default(), Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    memory::create(
        &db, "proj B item".into(), None,
        MemoryType::Prospective, MemorySubtype::ActionItem, Source::Manual,
        Some("proj-b".into()), vec![], vec![], Importance::default(), Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    let status = search::status_query(&db, Some("proj-a"), None).await.unwrap();
    assert_eq!(status.open_action_items.len(), 1);
    assert_eq!(status.open_action_items[0].summary, "proj A item");
}

#[tokio::test]
async fn status_empty_db() {
    let (db, _dir) = test_db().await;

    let status = search::status_query(&db, None, None).await.unwrap();
    assert!(status.open_action_items.is_empty());
    assert!(status.open_questions.is_empty());
    assert!(status.recent_decisions.is_empty());
    assert_eq!(status.total_memories, 0);
    assert_eq!(status.active_memories, 0);
}

// ═══════════════════════════════════════════════════════════════════════════
// ALERT TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn alerts_detect_overdue_action_items() {
    let (db, _dir) = test_db().await;

    // Create an action item with event_at 3 days ago
    memory::create(
        &db, "Ship feature X".into(), None,
        MemoryType::Prospective, MemorySubtype::ActionItem, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::default(),
        Some(Utc::now() - Duration::days(3)),
        vec![], None, None, None,
    ).await.unwrap();

    let config = AlertConfig {
        overdue_action_item_days: 1,
        stale_contact_days: 14,
        expiring_memory_hours: 24,
    };

    let alerts = alerts::check_all(&db, &config).await.unwrap();
    assert!(!alerts.is_empty(), "should detect overdue action item");
    assert!(alerts.iter().any(|a| a.alert_type == kb_server::types::AlertType::OverdueActionItem));
}

// ═══════════════════════════════════════════════════════════════════════════
// CONTRADICTION / UNSUPERSEDED UPDATE DETECTION
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn contradiction_alert_detects_unsuperseded_updates() {
    let (db, _dir) = test_db().await;

    // Create old memory and new memory with updates edge, but DON'T supersede
    let old_id = memory::create(
        &db, "We will use Postgres".into(), None,
        MemoryType::Semantic, MemorySubtype::Decision, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    let new_id = memory::create(
        &db, "We will use SurrealDB".into(), None,
        MemoryType::Semantic, MemorySubtype::Decision, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    // Create the updates edge (new updates old) but DON'T call supersede
    graph::link_memories(&db, &new_id, &old_id, RelationType::Updates).await.unwrap();

    let config = AlertConfig::default();
    let alerts = alerts::check_all(&db, &config).await.unwrap();

    assert!(
        alerts.iter().any(|a| a.alert_type == kb_server::types::AlertType::Contradiction),
        "should detect unsuperseded memory as potential contradiction"
    );
    assert!(
        alerts.iter().any(|a| a.reference_id == old_id.as_str()),
        "alert should reference the OLD (unsuperseded) memory"
    );
}

#[tokio::test]
async fn no_contradiction_when_properly_superseded() {
    let (db, _dir) = test_db().await;

    let old_id = memory::create(
        &db, "Old decision".into(), None,
        MemoryType::Semantic, MemorySubtype::Decision, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    let new_id = memory::create(
        &db, "New decision".into(), None,
        MemoryType::Semantic, MemorySubtype::Decision, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    // Properly supersede: edge + status change
    graph::link_memories(&db, &new_id, &old_id, RelationType::Updates).await.unwrap();
    memory::supersede(&db, &old_id).await.unwrap();

    let config = AlertConfig::default();
    let alerts = alerts::check_all(&db, &config).await.unwrap();

    assert!(
        !alerts.iter().any(|a| a.alert_type == kb_server::types::AlertType::Contradiction),
        "no contradiction alert when properly superseded"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// EXPIRING MEMORY ALERT TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn alerts_detect_expiring_memories() {
    let (db, _dir) = test_db().await;

    // Create a scratch memory (48h TTL) and manually set expires_at to soon
    let id = memory::create(
        &db, "scratch note about to expire".into(), None,
        MemoryType::Working, MemorySubtype::Scratch, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![], None, None, None,
    ).await.unwrap();

    // Set expires_at to 6 hours from now (within 24h alert window)
    db.query("UPDATE type::record('memory', $id) SET expires_at = time::now() + 6h")
        .bind(("id", id.as_str().to_string()))
        .await.unwrap();

    let config = AlertConfig {
        overdue_action_item_days: 1,
        stale_contact_days: 14,
        expiring_memory_hours: 24,
    };

    let alerts = alerts::check_all(&db, &config).await.unwrap();
    assert!(
        alerts.iter().any(|a| a.alert_type == kb_server::types::AlertType::ExpiringMemory),
        "should detect memory expiring within 24h"
    );
}

#[tokio::test]
async fn alerts_ignores_memories_expiring_beyond_window() {
    let (db, _dir) = test_db().await;

    memory::create(
        &db, "not expiring soon".into(), None,
        MemoryType::Working, MemorySubtype::Priority, Source::Manual,
        None, vec![], vec![], Importance::default(),
        Visibility::default(), None, vec![],
        Some(Utc::now() + Duration::days(5)), // expires in 5 days
        None, None,
    ).await.unwrap();

    let config = AlertConfig {
        overdue_action_item_days: 1,
        stale_contact_days: 14,
        expiring_memory_hours: 24,
    };

    let alerts = alerts::check_all(&db, &config).await.unwrap();
    assert!(
        !alerts.iter().any(|a| a.alert_type == kb_server::types::AlertType::ExpiringMemory),
        "should NOT alert for memories expiring beyond window"
    );
}
