use std::sync::Arc;

use chrono::{Duration, Utc};
use smriti::config::AlertConfig;
use smriti::shutdown::TaskRegistry;
use smriti::tools::{alerts, archive, consolidate, drill, entity, link, promote, recall,
                       recall_at, remember, status};
use smriti::types::{AlertType, EntityKind, *};

mod common;
use common::test_db;

fn remember_input(summary: &str, typ: MemoryType, sub: MemorySubtype) -> remember::RememberInput {
    // Pad short summaries to satisfy the 10-char validation gate in tests
    let padded = if summary.len() < 10 {
        format!("{summary} (test memory data)")
    } else {
        summary.to_string()
    };
    remember::RememberInput {
        summary: padded,
        memory_type: typ,
        subtype: sub,
        channel: Some(Channel::Cli),
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
    }
}

fn tasks() -> Arc<TaskRegistry> {
    Arc::new(TaskRegistry::new())
}

// ═══════════════════════════════════════════════════════════════════════════
// REMEMBER
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn remember_basic_stores_memory() {
    let (db, _dir) = test_db().await;

    let out = remember::execute(
        &db,
        None,
        None,
        tasks(),
        remember_input("Decided to use SurrealDB", MemoryType::Semantic, MemorySubtype::Decision),
    )
    .await
    .expect("remember failed");

    assert!(!out.id.is_empty());
    assert_eq!(out.message, "Memory stored");

    // Confirm it's retrievable
    let results = recall::execute(
        &db,
        None,
        None,
        recall::RecallInput {
            query: "SurrealDB".to_string(),
            memory_type: None,
            project: None,
            domain: None,
            person: None,
            since: None,
            until: None,
            limit: None,
            author_filter: None,
            min_score: None,
            channel: None,
            modality: None,
            rerank: None,
            spreading: None,
        },
        &Default::default(),
        &Default::default(),
    )
    .await
    .expect("recall failed");

    assert_eq!(results.count, 1);
    assert_eq!(results.memories[0].summary, "Decided to use SurrealDB");
}

#[tokio::test]
async fn remember_with_full_fields() {
    let (db, _dir) = test_db().await;

    let input = remember::RememberInput {
        summary: "Completed sprint planning".to_string(),
        memory_type: MemoryType::Episodic,
        subtype: MemorySubtype::Meeting,
        channel: Some(Channel::Cli),
        modality: Some(Modality::Text),
        content: Some("Full transcript here".to_string()),
        project: Some("atlas".to_string()),
        domain: Some(vec!["startup".to_string()]),
        people: None,
        importance: Some(4),
        visibility: Some(Visibility::Standard),
        event_at: None,
        tags: Some(vec!["sprint".to_string(), "planning".to_string()]),
        updates_id: None,
        source_agent: None,
        is_static: None,
    };

    let out = remember::execute(&db, None, None, tasks(), input).await.expect("remember failed");
    assert!(!out.id.is_empty());

    // Drill to verify content was stored
    let drilled = drill::execute(
        &db,
        drill::DrillInput { id: out.id.clone(), project: None },
    )
    .await
    .expect("drill failed");

    assert_eq!(drilled.memory.content.as_deref(), Some("Full transcript here"));
    assert_eq!(drilled.memory.project.as_deref(), Some("atlas"));
    assert_eq!(drilled.memory.importance.value(), 4);
    assert!(drilled.memory.tags.contains(&"sprint".to_string()));
}

#[tokio::test]
async fn remember_invalid_importance_returns_error() {
    let (db, _dir) = test_db().await;

    let mut input = remember_input("test", MemoryType::Semantic, MemorySubtype::Fact);
    input.importance = Some(6); // out of range 1-5

    let result = remember::execute(&db, None, None, tasks(), input).await;
    assert!(result.is_err(), "expected error for importance=6");
}

#[tokio::test]
async fn remember_invalid_subtype_returns_error() {
    let (db, _dir) = test_db().await;

    // meeting subtype is only valid for Episodic, not Semantic
    let input = remember_input("test", MemoryType::Semantic, MemorySubtype::Meeting);
    let result = remember::execute(&db, None, None, tasks(), input).await;
    assert!(result.is_err(), "Semantic/Meeting should be invalid");
}

#[tokio::test]
async fn remember_with_people_creates_entity_links() {
    let (db, _dir) = test_db().await;

    let mut input = remember_input(
        "Met with Jane about fundraising",
        MemoryType::Episodic,
        MemorySubtype::Meeting,
    );
    input.people = Some(vec!["Jane Park".to_string()]);

    remember::execute(&db, None, None, tasks(), input).await.expect("remember failed");

    // Entity should exist and have memory_count >= 1
    let found = entity::execute_find(
        &db,
        entity::EntityFindInput {
            query: "Jane".to_string(),
            kind: None,
            role: None,
        },
    )
    .await
    .expect("entity find failed");

    assert_eq!(found.count, 1);
    assert_eq!(found.entities[0].name, "Jane Park");
    assert!(found.entities[0].memory_count >= 1);
}

#[tokio::test]
async fn remember_supersession_archives_old_memory() {
    let (db, _dir) = test_db().await;

    // Create original decision
    let original = remember::execute(
        &db,
        None,
        None,
        tasks(),
        remember_input("We will use Postgres", MemoryType::Semantic, MemorySubtype::Decision),
    )
    .await
    .expect("original failed");

    // Supersede with new decision
    let mut new_input = remember_input(
        "We will use SurrealDB instead",
        MemoryType::Semantic,
        MemorySubtype::Decision,
    );
    new_input.updates_id = Some(original.id.clone());
    remember::execute(&db, None, None, tasks(), new_input).await.expect("new failed");

    // Original should no longer appear in recall
    let results = recall::execute(
        &db,
        None,
        None,
        recall::RecallInput {
            query: "Postgres".to_string(),
            memory_type: None,
            project: None,
            domain: None,
            person: None,
            since: None,
            until: None,
            limit: None,
            author_filter: None,
            min_score: None,
            channel: None,
            modality: None,
            rerank: None,
            spreading: None,
        },
        &Default::default(),
        &Default::default(),
    )
    .await
    .expect("recall failed");

    assert!(
        results.memories.iter().all(|m| m.id.as_str() != original.id),
        "superseded memory should not appear in recall"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// RECALL
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn recall_empty_returns_all() {
    let (db, _dir) = test_db().await;

    for summary in ["Alpha decision", "Beta insight", "Gamma fact"] {
        remember::execute(
            &db,
            None,
            None,
            tasks(),
            remember_input(summary, MemoryType::Semantic, MemorySubtype::Decision),
        )
        .await
        .unwrap();
    }

    let out = recall::execute(
        &db,
        None,
        None,
        recall::RecallInput {
            query: String::new(),
            memory_type: None,
            project: None,
            domain: None,
            person: None,
            since: None,
            until: None,
            limit: None,
            author_filter: None,
            min_score: None,
            channel: None,
            modality: None,
            rerank: None,
            spreading: None,
        },
        &Default::default(),
        &Default::default(),
    )
    .await
    .expect("recall failed");

    assert_eq!(out.count, 3);
}

#[tokio::test]
async fn recall_filters_by_project() {
    let (db, _dir) = test_db().await;

    let mut a = remember_input("Atlas memory", MemoryType::Semantic, MemorySubtype::Fact);
    a.project = Some("atlas".to_string());
    let mut b = remember_input("Beacon memory", MemoryType::Semantic, MemorySubtype::Fact);
    b.project = Some("beacon".to_string());
    remember::execute(&db, None, None, tasks(), a).await.unwrap();
    remember::execute(&db, None, None, tasks(), b).await.unwrap();

    let out = recall::execute(
        &db,
        None,
        None,
        recall::RecallInput {
            query: String::new(),
            memory_type: None,
            project: Some("atlas".to_string()),
            domain: None,
            person: None,
            since: None,
            until: None,
            limit: None,
            author_filter: None,
            min_score: None,
            channel: None,
            modality: None,
            rerank: None,
            spreading: None,
        },
        &Default::default(),
        &Default::default(),
    )
    .await
    .unwrap();

    assert_eq!(out.count, 1);
    assert_eq!(out.memories[0].project.as_deref(), Some("atlas"));
}

#[tokio::test]
async fn recall_filters_by_domain() {
    let (db, _dir) = test_db().await;

    let mut health = remember_input("Morning run PR", MemoryType::Semantic, MemorySubtype::Metric);
    health.domain = Some(vec!["health".to_string()]);
    let mut work = remember_input("Closed Series A", MemoryType::Semantic, MemorySubtype::Decision);
    work.domain = Some(vec!["startup".to_string()]);
    remember::execute(&db, None, None, tasks(), health).await.unwrap();
    remember::execute(&db, None, None, tasks(), work).await.unwrap();

    let out = recall::execute(
        &db,
        None,
        None,
        recall::RecallInput {
            query: String::new(),
            memory_type: None,
            project: None,
            domain: Some("health".to_string()),
            person: None,
            since: None,
            until: None,
            limit: None,
            author_filter: None,
            min_score: None,
            channel: None,
            modality: None,
            rerank: None,
            spreading: None,
        },
        &Default::default(),
        &Default::default(),
    )
    .await
    .unwrap();

    assert_eq!(out.count, 1);
    assert!(out.memories[0].summary.starts_with("Morning run PR"));
}

#[tokio::test]
async fn recall_limit_respected() {
    let (db, _dir) = test_db().await;

    for i in 0..10 {
        remember::execute(
            &db,
            None,
            None,
            tasks(),
            remember_input(
                &format!("Memory {i}"),
                MemoryType::Semantic,
                MemorySubtype::Fact,
            ),
        )
        .await
        .unwrap();
    }

    let out = recall::execute(
        &db,
        None,
        None,
        recall::RecallInput {
            query: String::new(),
            memory_type: None,
            project: None,
            domain: None,
            person: None,
            since: None,
            until: None,
            limit: Some(3),
            author_filter: None,
            min_score: None,
            channel: None,
            modality: None,
            rerank: None,
            spreading: None,
        },
        &Default::default(),
        &Default::default(),
    )
    .await
    .unwrap();

    assert_eq!(out.count, 3);
}

#[tokio::test]
async fn recall_limit_capped_at_25() {
    let (db, _dir) = test_db().await;

    for i in 0..30 {
        remember::execute(
            &db,
            None,
            None,
            tasks(),
            remember_input(
                &format!("Memory {i}"),
                MemoryType::Semantic,
                MemorySubtype::Fact,
            ),
        )
        .await
        .unwrap();
    }

    let out = recall::execute(
        &db,
        None,
        None,
        recall::RecallInput {
            query: String::new(),
            memory_type: None,
            project: None,
            domain: None,
            person: None,
            since: None,
            until: None,
            limit: Some(100), // exceeds cap
            author_filter: None,
            min_score: None,
            channel: None,
            modality: None,
            rerank: None,
            spreading: None,
        },
        &Default::default(),
        &Default::default(),
    )
    .await
    .unwrap();

    assert!(out.count <= 25, "recall should cap at 25, got {}", out.count);
}

#[tokio::test]
async fn recall_time_range_filter() {
    let (db, _dir) = test_db().await;

    let past = Utc::now() - Duration::days(30);
    let recent = Utc::now() - Duration::days(1);

    let mut old = remember_input("Old memory", MemoryType::Semantic, MemorySubtype::Fact);
    old.event_at = Some(past);
    let mut new = remember_input("Recent memory", MemoryType::Semantic, MemorySubtype::Fact);
    new.event_at = Some(recent);
    remember::execute(&db, None, None, tasks(), old).await.unwrap();
    remember::execute(&db, None, None, tasks(), new).await.unwrap();

    let out = recall::execute(
        &db,
        None,
        None,
        recall::RecallInput {
            query: String::new(),
            memory_type: None,
            project: None,
            domain: None,
            person: None,
            since: Some(Utc::now() - Duration::days(7)),
            until: None,
            limit: None,
            author_filter: None,
            min_score: None,
            channel: None,
            modality: None,
            rerank: None,
            spreading: None,
        },
        &Default::default(),
        &Default::default(),
    )
    .await
    .unwrap();

    assert_eq!(out.count, 1);
    assert!(out.memories[0].summary.starts_with("Recent memory"));
}

// ═══════════════════════════════════════════════════════════════════════════
// RECALL_AT (point-in-time)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn recall_at_filters_by_created_at() {
    let (db, _dir) = test_db().await;

    // Both memories are created NOW. recall_at uses created_at (when stored),
    // not event_at (when the event happened) — bi-temporal design.
    remember::execute(
        &db,
        None,
        None,
        tasks(),
        remember_input("Some fact", MemoryType::Semantic, MemorySubtype::Fact),
    )
    .await
    .unwrap();

    // as_of = future → both memories are visible
    let future_out = recall_at::execute(
        &db,
        None,
        None,
        recall_at::RecallAtInput {
            query: String::new(),
            as_of: Utc::now() + Duration::days(1),
            project: None,
            domain: None,
            limit: None,
            author_filter: None,
            channel: None,
            modality: None,
            exclude_retracted: None,
            spreading: None,
        },
        &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(future_out.count, 1);

    // as_of = past → memory was created after this point → not visible
    let past_out = recall_at::execute(
        &db,
        None,
        None,
        recall_at::RecallAtInput {
            query: String::new(),
            as_of: Utc::now() - Duration::days(1),
            project: None,
            domain: None,
            limit: None,
            author_filter: None,
            channel: None,
            modality: None,
            exclude_retracted: None,
            spreading: None,
        },
        &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(past_out.count, 0);
}

// ═══════════════════════════════════════════════════════════════════════════
// DRILL
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn drill_returns_full_content() {
    let (db, _dir) = test_db().await;

    let mut input = remember_input("Standup notes", MemoryType::Episodic, MemorySubtype::DailyLog);
    input.content = Some("Detailed standup content here".to_string());

    let stored = remember::execute(&db, None, None, tasks(), input).await.unwrap();

    let out = drill::execute(&db, drill::DrillInput { id: stored.id, project: None }).await.unwrap();
    assert_eq!(out.memory.content.as_deref(), Some("Detailed standup content here"));
}

#[tokio::test]
async fn drill_increments_access_count() {
    let (db, _dir) = test_db().await;

    let stored = remember::execute(
        &db,
        None,
        None,
        tasks(),
        remember_input("Access-counted memory", MemoryType::Semantic, MemorySubtype::Fact),
    )
    .await
    .unwrap();

    // Drill twice
    drill::execute(&db, drill::DrillInput { id: stored.id.clone(), project: None }).await.unwrap();
    let out = drill::execute(&db, drill::DrillInput { id: stored.id, project: None }).await.unwrap();

    assert_eq!(out.memory.access_count, 2);
}

#[tokio::test]
async fn drill_nonexistent_returns_error() {
    let (db, _dir) = test_db().await;

    let result = drill::execute(
        &db,
        drill::DrillInput { id: "mem_nonexistent".to_string(), project: None },
    )
    .await;

    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// ARCHIVE (replaces kb_forget + kb_retract)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn forget_archives_memory() {
    let (db, _dir) = test_db().await;

    let stored = remember::execute(
        &db,
        None,
        None,
        tasks(),
        remember_input("To be forgotten", MemoryType::Working, MemorySubtype::Scratch),
    )
    .await
    .unwrap();

    archive::execute(&db, archive::ArchiveInput {
        id: stored.id.clone(),
        reason: ArchiveReason::Forget,
        correction_id: None,
    }).await.unwrap();

    // Memory should not appear in recall
    let results = recall::execute(
        &db,
        None,
        None,
        recall::RecallInput {
            query: "forgotten".to_string(),
            memory_type: None,
            project: None,
            domain: None,
            person: None,
            since: None,
            until: None,
            limit: None,
            author_filter: None,
            min_score: None,
            channel: None,
            modality: None,
            rerank: None,
            spreading: None,
        },
        &Default::default(),
        &Default::default(),
    )
    .await
    .unwrap();

    assert!(
        results.memories.iter().all(|m| m.id.as_str() != stored.id),
        "archived memory should not appear in recall"
    );
}

#[tokio::test]
async fn forget_nonexistent_returns_error() {
    let (db, _dir) = test_db().await;

    let result = archive::execute(
        &db,
        archive::ArchiveInput { id: "mem_ghost".to_string(), reason: ArchiveReason::Forget, correction_id: None },
    )
    .await;

    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// PROMOTE
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn promote_clears_ttl_and_bumps_importance() {
    let (db, _dir) = test_db().await;

    // Working/scratch has TTL and low importance
    let stored = remember::execute(
        &db,
        None,
        None,
        tasks(),
        remember_input("Quick scratch note", MemoryType::Working, MemorySubtype::Scratch),
    )
    .await
    .unwrap();

    promote::execute(&db, promote::PromoteInput { id: stored.id.clone() }).await.unwrap();

    let drilled = drill::execute(&db, drill::DrillInput { id: stored.id, project: None }).await.unwrap();

    // TTL should be cleared
    assert!(drilled.memory.expires_at.is_none(), "expires_at should be None after promote");
    // Importance should be >= 3
    assert!(drilled.memory.importance.value() >= 3);
}

// ═══════════════════════════════════════════════════════════════════════════
// LINK
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn link_creates_relation_between_memories() {
    let (db, _dir) = test_db().await;

    let a = remember::execute(
        &db,
        None,
        None,
        tasks(),
        remember_input("Base insight", MemoryType::Semantic, MemorySubtype::Pattern),
    )
    .await
    .unwrap();

    let b = remember::execute(
        &db,
        None,
        None,
        tasks(),
        remember_input("Extended insight", MemoryType::Semantic, MemorySubtype::Pattern),
    )
    .await
    .unwrap();

    let out = link::execute(
        &db,
        link::LinkInput {
            from_id: b.id.clone(),
            to_id: a.id.clone(),
            relation: RelationType::Extends,
        },
    )
    .await
    .unwrap();

    assert!(out.message.contains("Linked"));
}

#[tokio::test]
async fn link_all_relation_types() {
    let (db, _dir) = test_db().await;

    for rel in [
        RelationType::Updates,
        RelationType::Extends,
        RelationType::Derives,
        RelationType::Relates,
    ] {
        let a = remember::execute(
            &db,
            None,
            None,
            tasks(),
            remember_input("A", MemoryType::Semantic, MemorySubtype::Fact),
        )
        .await
        .unwrap();
        let b = remember::execute(
            &db,
            None,
            None,
            tasks(),
            remember_input("B", MemoryType::Semantic, MemorySubtype::Fact),
        )
        .await
        .unwrap();

        link::execute(
            &db,
            link::LinkInput {
                from_id: a.id,
                to_id: b.id,
                relation: rel,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("link with {rel:?} failed: {e}"));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// CONSOLIDATE
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn consolidate_merges_and_archives_originals() {
    let (db, _dir) = test_db().await;

    let a = remember::execute(
        &db,
        None,
        None,
        tasks(),
        remember_input("Sprint log day 1", MemoryType::Episodic, MemorySubtype::DailyLog),
    )
    .await
    .unwrap();
    let b = remember::execute(
        &db,
        None,
        None,
        tasks(),
        remember_input("Sprint log day 2", MemoryType::Episodic, MemorySubtype::DailyLog),
    )
    .await
    .unwrap();
    let c = remember::execute(
        &db,
        None,
        None,
        tasks(),
        remember_input("Sprint log day 3", MemoryType::Episodic, MemorySubtype::DailyLog),
    )
    .await
    .unwrap();

    let out = consolidate::execute(
        &db,
        None,
        consolidate::ConsolidateInput {
            ids: vec![a.id.clone(), b.id.clone(), c.id.clone()],
            result_summary: "Sprint week summary: shipped auth flow".to_string(),
        },
    )
    .await
    .unwrap();

    assert!(!out.id.is_empty());
    assert_eq!(out.message, "Consolidated 3 memories");

    // Consolidated memory should be retrievable
    let drilled = drill::execute(&db, drill::DrillInput { id: out.id, project: None }).await.unwrap();
    assert_eq!(drilled.memory.summary, "Sprint week summary: shipped auth flow");

    // Originals should be archived (not in recall)
    let results = recall::execute(
        &db,
        None,
        None,
        recall::RecallInput {
            query: "Sprint log".to_string(),
            memory_type: None,
            project: None,
            domain: None,
            person: None,
            since: None,
            until: None,
            limit: None,
            author_filter: None,
            min_score: None,
            channel: None,
            modality: None,
            rerank: None,
            spreading: None,
        },
        &Default::default(),
        &Default::default(),
    )
    .await
    .unwrap();

    let ids: Vec<_> = results.memories.iter().map(|m| m.id.as_str()).collect();
    assert!(!ids.contains(&a.id.as_str()), "original a should be archived");
    assert!(!ids.contains(&b.id.as_str()), "original b should be archived");
    assert!(!ids.contains(&c.id.as_str()), "original c should be archived");
}

#[tokio::test]
async fn consolidate_empty_ids_returns_error() {
    let (db, _dir) = test_db().await;

    let result = consolidate::execute(
        &db,
        None,
        consolidate::ConsolidateInput {
            ids: vec![],
            result_summary: "nothing".to_string(),
        },
    )
    .await;

    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// ENTITY
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn entity_upsert_creates_and_retrieves() {
    let (db, _dir) = test_db().await;

    let out = entity::execute_upsert(
        &db,
        entity::EntityUpsertInput {
            name: "Jane Park".to_string(),
            kind: "person".to_string(),
            roles: Some(vec!["investor".to_string()]),
            attrs: Some(serde_json::json!({"fund": "Sequoia", "check_size": "$500k"})),
            context: Some("Met at demo day".to_string()),
            aliases: None,
        },
    )
    .await
    .expect("upsert failed");

    assert!(!out.id.is_empty());

    let got = entity::execute_get(
        &db,
        entity::EntityGetInput { id: out.id },
    )
    .await
    .expect("get failed");

    assert_eq!(got.entity.name, "Jane Park");
    assert_eq!(got.entity.kind, EntityKind::Person);
    assert_eq!(got.entity.roles, vec!["investor"]);
    assert_eq!(got.entity.context.as_deref(), Some("Met at demo day"));
    assert_eq!(got.entity.attrs["fund"], "Sequoia");
}

#[tokio::test]
async fn entity_upsert_is_idempotent() {
    let (db, _dir) = test_db().await;

    let first = entity::execute_upsert(
        &db,
        entity::EntityUpsertInput {
            name: "Alice".to_string(),
            kind: "person".to_string(),
            roles: Some(vec![]),
            attrs: None,
            context: None,
            aliases: None,
        },
    )
    .await
    .unwrap();

    let second = entity::execute_upsert(
        &db,
        entity::EntityUpsertInput {
            name: "Alice".to_string(),
            kind: "person".to_string(),
            roles: Some(vec!["engineer".to_string()]),
            attrs: Some(serde_json::json!({"team": "backend"})),
            context: Some("Updated context".to_string()),
            aliases: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(first.id, second.id, "same entity should be returned on update");

    let got = entity::execute_get(&db, entity::EntityGetInput { id: first.id })
        .await
        .unwrap();

    assert_eq!(got.entity.roles, vec!["engineer"]);
    assert_eq!(got.entity.context.as_deref(), Some("Updated context"));
    assert_eq!(got.entity.attrs["team"], "backend");
}

#[tokio::test]
async fn entity_find_fuzzy_and_filtered() {
    let (db, _dir) = test_db().await;

    entity::execute_upsert(&db, entity::EntityUpsertInput {
        name: "Alice Johnson".to_string(), kind: "person".to_string(),
        roles: Some(vec!["engineer".to_string()]), attrs: None, context: None, aliases: None,
    }).await.unwrap();

    entity::execute_upsert(&db, entity::EntityUpsertInput {
        name: "Bob Smith".to_string(), kind: "person".to_string(),
        roles: Some(vec!["designer".to_string()]), attrs: None, context: None, aliases: None,
    }).await.unwrap();

    entity::execute_upsert(&db, entity::EntityUpsertInput {
        name: "Acme Corp".to_string(), kind: "company".to_string(),
        roles: Some(vec!["customer".to_string()]), attrs: None, context: None, aliases: None,
    }).await.unwrap();

    // Fuzzy name
    let r = entity::execute_find(&db, entity::EntityFindInput {
        query: "alice".to_string(), kind: None, role: None,
    }).await.unwrap();
    assert_eq!(r.count, 1);
    assert_eq!(r.entities[0].name, "Alice Johnson");

    // Kind filter
    let r = entity::execute_find(&db, entity::EntityFindInput {
        query: "".to_string(), kind: Some("company".to_string()), role: None,
    }).await.unwrap();
    assert_eq!(r.count, 1);
    assert_eq!(r.entities[0].name, "Acme Corp");

    // Role filter
    let r = entity::execute_find(&db, entity::EntityFindInput {
        query: "".to_string(), kind: None, role: Some("designer".to_string()),
    }).await.unwrap();
    assert_eq!(r.count, 1);
    assert_eq!(r.entities[0].name, "Bob Smith");
}

#[tokio::test]
async fn entity_get_includes_memory_count() {
    let (db, _dir) = test_db().await;

    // Create an entity via remember (person linking)
    let mut input = remember_input("Meeting with Carol", MemoryType::Episodic, MemorySubtype::Meeting);
    input.people = Some(vec!["Carol Chen".to_string()]);
    remember::execute(&db, None, None, tasks(), input).await.unwrap();

    let found = entity::execute_find(&db, entity::EntityFindInput {
        query: "Carol".to_string(), kind: None, role: None,
    }).await.unwrap();
    assert_eq!(found.count, 1);

    let got = entity::execute_get(&db, entity::EntityGetInput {
        id: found.entities[0].id.as_str().to_string(),
    }).await.unwrap();

    assert_eq!(got.entity.memory_count, 1);
}

#[tokio::test]
async fn entity_get_nonexistent_returns_error() {
    let (db, _dir) = test_db().await;

    let result = entity::execute_get(
        &db,
        entity::EntityGetInput { id: "ent_does_not_exist".to_string() },
    )
    .await;

    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// STATUS
// ═══════════════════════════════════════════════════════════════════════════

fn status_ctx() -> status::DiagnosticsCtx<'static> {
    static DEFAULT_SPREADING: std::sync::LazyLock<smriti::config::SpreadingConfig> =
        std::sync::LazyLock::new(smriti::config::SpreadingConfig::default);
    status::DiagnosticsCtx { embedder: None, sidecar: None, spreading_config: &DEFAULT_SPREADING }
}

fn status_input(project: Option<String>, domain: Option<String>) -> status::StatusInput {
    status::StatusInput { project, domain, include_diagnostics: None }
}

#[tokio::test]
async fn status_empty_db() {
    let (db, _dir) = test_db().await;

    let report = status::execute(&db, status_ctx(), status_input(None, None))
        .await
        .unwrap();

    assert_eq!(report.active_memories, 0);
    assert!(report.open_action_items.is_empty());
    assert!(report.recent_decisions.is_empty());
    assert!(report.diagnostics.is_none());
}

#[tokio::test]
async fn status_counts_active_memories() {
    let (db, _dir) = test_db().await;

    for _ in 0..5 {
        remember::execute(
            &db,
            None,
            None,
            tasks(),
            remember_input("Some fact", MemoryType::Semantic, MemorySubtype::Fact),
        )
        .await
        .unwrap();
    }

    let report = status::execute(&db, status_ctx(), status_input(None, None))
        .await
        .unwrap();

    assert_eq!(report.active_memories, 5);
}

#[tokio::test]
async fn status_includes_action_items() {
    let (db, _dir) = test_db().await;

    let mut input = remember_input("Follow up with investor", MemoryType::Prospective, MemorySubtype::ActionItem);
    input.project = Some("atlas".to_string());
    remember::execute(&db, None, None, tasks(), input).await.unwrap();

    let report = status::execute(
        &db,
        status_ctx(),
        status_input(Some("atlas".to_string()), None),
    ).await.unwrap();

    assert!(!report.open_action_items.is_empty());
    assert_eq!(report.open_action_items[0].summary, "Follow up with investor");
}

#[tokio::test]
async fn status_filters_by_project() {
    let (db, _dir) = test_db().await;

    let mut atlas = remember_input("Atlas decision", MemoryType::Semantic, MemorySubtype::Decision);
    atlas.project = Some("atlas".to_string());
    let mut beacon = remember_input("Beacon decision", MemoryType::Semantic, MemorySubtype::Decision);
    beacon.project = Some("beacon".to_string());
    remember::execute(&db, None, None, tasks(), atlas).await.unwrap();
    remember::execute(&db, None, None, tasks(), beacon).await.unwrap();

    let report = status::execute(
        &db,
        status_ctx(),
        status_input(Some("atlas".to_string()), None),
    ).await.unwrap();

    // Only atlas memory should count
    assert_eq!(report.active_memories, 1);
    assert!(report.recent_decisions.iter().all(|d| d.project.as_deref() == Some("atlas")));
}

#[tokio::test]
async fn status_diagnostics_block_includes_version_and_db_counts() {
    let (db, _dir) = test_db().await;

    remember::execute(
        &db,
        None,
        None,
        tasks(),
        remember_input("A fact", MemoryType::Semantic, MemorySubtype::Fact),
    )
    .await
    .unwrap();

    let report = status::execute(
        &db,
        status_ctx(),
        status::StatusInput { project: None, domain: None, include_diagnostics: Some(true) },
    )
    .await
    .unwrap();

    let d = report.diagnostics.expect("diagnostics block must be present");
    assert!(!d.version.is_empty());
    assert_eq!(d.db.active_memories, 1);
    // Embedder and sidecar are optional and disabled in this test fixture.
    assert!(d.embedder.is_none());
    assert!(d.sidecar.is_none());
}

// ═══════════════════════════════════════════════════════════════════════════
// ALERTS
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn alerts_empty_db_returns_none() {
    let (db, _dir) = test_db().await;

    let out = alerts::execute(&db, &AlertConfig::default()).await.unwrap();
    assert_eq!(out.count, 0);
}

#[tokio::test]
async fn alerts_detects_overdue_action_items() {
    let (db, _dir) = test_db().await;

    // Action item with event_at in the past
    let mut input = remember_input(
        "Overdue task: send deck",
        MemoryType::Prospective,
        MemorySubtype::ActionItem,
    );
    input.event_at = Some(Utc::now() - Duration::days(3));
    remember::execute(&db, None, None, tasks(), input).await.unwrap();

    let out = alerts::execute(&db, &AlertConfig::default()).await.unwrap();

    assert!(out.count >= 1);
    assert!(out.alerts.iter().any(|a| matches!(a.alert_type, AlertType::OverdueActionItem)));
}

#[tokio::test]
async fn alerts_ignores_future_action_items() {
    let (db, _dir) = test_db().await;

    let mut input = remember_input(
        "Future task: quarterly review",
        MemoryType::Prospective,
        MemorySubtype::ActionItem,
    );
    input.event_at = Some(Utc::now() + Duration::days(7));
    remember::execute(&db, None, None, tasks(), input).await.unwrap();

    let out = alerts::execute(&db, &AlertConfig::default()).await.unwrap();

    assert!(
        out.alerts.iter().all(|a| !matches!(a.alert_type, AlertType::OverdueActionItem)),
        "future action item should not fire alert"
    );
}
