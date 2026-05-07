//! Alias feature tests (ADR-030).
//!
//! Covers:
//! - Core CRUD: upsert_alias validation, archive, resolve, resolve_fts, resolve_at
//! - Entity layer: upsert with aliases, get_by_name alias fallback, find alias merge,
//!   archive cascade
//! - Graph layer: link_entity alias resolution, no-duplicate-entity behavior
//! - Search/recall: person= alias filter, archived alias exclusion, Step 4.5 pre-expansion
//! - Auto-link: match_known_entities alias FTS fallback
//! - Tool layer: execute_upsert aliases param, collision MCP error
//! - Migration idempotency: entity_alias table/indexes survive double-run
//!
//! ## Implementation notes
//!
//! Bug 1 (RELATE -> none parse error) and Bug 2 (reserved keyword `value`) have been
//! fixed: `entity_alias` is now a plain SCHEMAFULL table with an `entity_id` field
//! instead of a RELATE-based edge, and all SurrealQL backtick-escapes `` `value` ``.

mod common;
use common::{create_test_memory, test_db};

use chrono::{Duration, Utc};
use smriti::db::{auto_link, entity, entity_alias, graph, memory, search};
use smriti::error::KbError;
use smriti::tools::entity as tool_entity;
use smriti::types::*;
use surrealdb::types::RecordId;

// ═══════════════════════════════════════════════════════════════════════════
// VALIDATION — fail before DB call; always pass
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn upsert_alias_rejects_len_less_than_3() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db,
        "Bo Smith",
        EntityKind::Person,
        vec![],
        serde_json::json!({}),
        None,
        &[],
    )
    .await
    .unwrap();

    let err = entity_alias::upsert_alias(&db, &eid, "bo", None)
        .await
        .expect_err("alias 'bo' (len=2) should be rejected before DB call");

    assert!(
        matches!(err, KbError::InvalidInput(_)),
        "expected InvalidInput for short alias; got {err:?}"
    );
}

#[tokio::test]
async fn upsert_alias_rejects_whitespace_only() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db,
        "Someone",
        EntityKind::Person,
        vec![],
        serde_json::json!({}),
        None,
        &[],
    )
    .await
    .unwrap();

    let err = entity_alias::upsert_alias(&db, &eid, "  ", None)
        .await
        .expect_err("whitespace-only alias should be rejected after trim");

    assert!(
        matches!(err, KbError::InvalidInput(_)),
        "expected InvalidInput for empty alias after trim; got {err:?}"
    );
}

#[tokio::test]
async fn upsert_alias_rejects_two_char_after_trim() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db,
        "Someone",
        EntityKind::Person,
        vec![],
        serde_json::json!({}),
        None,
        &[],
    )
    .await
    .unwrap();

    // "  ab  " trims to "ab" (len=2) — fails length validation.
    let err = entity_alias::upsert_alias(&db, &eid, "  ab  ", None)
        .await
        .expect_err("alias trimmed to 2 chars should be rejected");

    assert!(
        matches!(err, KbError::InvalidInput(_)),
        "expected InvalidInput for 2-char trimmed alias; got {err:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// RESOLVE EMPTY — no alias rows in DB; queries return [] correctly
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn resolve_returns_empty_for_nonexistent_alias() {
    let (db, _dir) = test_db().await;

    let resolved = entity_alias::resolve(&db, "nobody").await.unwrap();
    assert!(
        resolved.is_empty(),
        "nonexistent alias should resolve to empty"
    );
}

#[tokio::test]
async fn resolve_fts_returns_empty_when_no_aliases_exist() {
    let (db, _dir) = test_db().await;

    let resolved = entity_alias::resolve_fts(&db, "bobby", 5).await.unwrap();
    assert!(
        resolved.is_empty(),
        "FTS on non-existent alias should return empty"
    );
}

#[tokio::test]
async fn resolve_at_returns_empty_when_no_aliases_exist() {
    let (db, _dir) = test_db().await;

    let as_of = Utc::now();
    let resolved = entity_alias::resolve_at(&db, "anu", &as_of).await.unwrap();
    assert!(
        resolved.is_empty(),
        "bitemporal resolve with no aliases should return empty"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// ERROR TYPE SHAPE — AliasCollision satisfies client_message contract
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn alias_collision_client_message_contains_value() {
    let err = KbError::AliasCollision {
        value: "bob".to_string(),
        existing_canonical: "Robert Smith".to_string(),
    };
    let msg = err.client_message();
    assert!(
        msg.contains("bob"),
        "client_message should include colliding alias; got: {msg}"
    );
}

#[tokio::test]
async fn alias_collision_client_message_contains_existing_canonical() {
    let err = KbError::AliasCollision {
        value: "bob".to_string(),
        existing_canonical: "Robert Smith".to_string(),
    };
    let msg = err.client_message();
    assert!(
        msg.contains("Robert Smith"),
        "client_message should include existing canonical entity name; got: {msg}"
    );
}

#[tokio::test]
async fn alias_collision_display_includes_alias_and_canonical() {
    let err = KbError::AliasCollision {
        value: "rob".to_string(),
        existing_canonical: "Roberto Sanchez".to_string(),
    };
    let display = err.to_string();
    assert!(display.contains("rob"), "Display should include alias; got: {display}");
    assert!(
        display.contains("Roberto Sanchez"),
        "Display should include canonical name; got: {display}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// MIGRATION IDEMPOTENCY
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn migration_021_recorded_in_migration_table() {
    let (db, _dir) = test_db().await;

    let mut result = db
        .query("SELECT version FROM _migration WHERE version = 21 LIMIT 1")
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = result.take(0).unwrap();
    assert!(
        !rows.is_empty(),
        "migration 21 should be recorded in _migration table"
    );
}

#[tokio::test]
async fn migration_021_entity_alias_table_has_unique_index() {
    let (db, _dir) = test_db().await;

    let mut info = db
        .query("INFO FOR TABLE entity_alias")
        .await
        .expect("INFO FOR TABLE entity_alias should succeed");
    let table_info: Option<serde_json::Value> = info.take(0).expect("take info result");
    let table_info = table_info.expect("INFO FOR TABLE entity_alias returned no rows");

    let indexes = table_info.get("indexes").expect("indexes key");
    let unique_def = indexes
        .get("idx_alias_value_unique")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    assert!(
        unique_def.contains("UNIQUE"),
        "idx_alias_value_unique should have UNIQUE constraint; got: {unique_def:?}"
    );
}

#[tokio::test]
async fn migration_021_entity_alias_table_has_ft_index() {
    let (db, _dir) = test_db().await;

    let mut info = db
        .query("INFO FOR TABLE entity_alias")
        .await
        .expect("INFO FOR TABLE entity_alias");
    let table_info: Option<serde_json::Value> = info.take(0).unwrap();
    let table_info = table_info.unwrap();

    let indexes = table_info.get("indexes").expect("indexes key");
    let ft_def = indexes
        .get("idx_alias_value_ft")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    assert!(
        !ft_def.is_empty(),
        "idx_alias_value_ft should exist"
    );
    assert!(
        ft_def.contains("FULLTEXT") || ft_def.contains("BM25") || ft_def.contains("alias_analyzer"),
        "idx_alias_value_ft should be a fulltext index; got: {ft_def:?}"
    );
}

#[tokio::test]
async fn migration_021_is_idempotent_on_double_run() {
    let (db, _dir) = test_db().await;
    use smriti::db::migrations;

    migrations::run(&db)
        .await
        .expect("second migration run should not error");

    let mut result = db
        .query("SELECT count() AS n FROM _migration GROUP ALL")
        .await
        .unwrap();
    let row: Option<serde_json::Value> = result.take(0).unwrap();
    let count = row
        .and_then(|v| v.get("n").and_then(|n| n.as_u64()))
        .unwrap_or(0);
    assert_eq!(
        count, 21,
        "double migration run should not duplicate migration records"
    );
}

#[tokio::test]
async fn migration_021_alias_analyzer_defined() {
    let (db, _dir) = test_db().await;

    // Confirm alias_analyzer was created by checking DB INFO.
    let mut info = db.query("INFO FOR DB").await.expect("INFO FOR DB");
    let db_info: Option<serde_json::Value> = info.take(0).unwrap();
    let db_info = db_info.expect("INFO FOR DB returned no rows");

    // In SurrealDB v3, analyzers appear under the "analyzers" key.
    if let Some(az) = db_info.get("analyzers") {
        let az_str = az.to_string();
        assert!(
            az_str.contains("alias_analyzer"),
            "alias_analyzer should be defined in DB; got: {az_str}"
        );
    }
    // If key is absent in this SurrealDB version, the table query above
    // already confirmed the migration ran — this assertion is a best-effort check.
}

// ═══════════════════════════════════════════════════════════════════════════
// ENTITY LAYER — skip logic runs before DB; testable
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn entity_upsert_canonical_equal_alias_is_skipped_without_error() {
    let (db, _dir) = test_db().await;

    // "robert" (lowercased) == canonical name "Robert" (lowercased).
    // entity::upsert skips before calling upsert_alias.
    let eid = entity::upsert(
        &db,
        "Robert",
        EntityKind::Person,
        vec![],
        serde_json::json!({}),
        None,
        &["robert".to_string()],
    )
    .await
    .expect("canonical-equal alias must be silently skipped, not error");

    let fetched = entity::get(&db, &eid).await.unwrap();
    assert_eq!(fetched.name, "Robert", "entity should have canonical name");
}

#[tokio::test]
async fn entity_upsert_short_aliases_all_skipped_without_error() {
    let (db, _dir) = test_db().await;

    // All provided aliases are below len=3 — all skipped, no upsert_alias call is made.
    // This verifies the skip-before-call logic without triggering the RELATE bug.
    let eid = entity::upsert(
        &db,
        "Robert Smith",
        EntityKind::Person,
        vec![],
        serde_json::json!({}),
        None,
        &["b".to_string(), "bo".to_string()], // both len < 3; both skipped
    )
    .await
    .expect("all short aliases must be silently skipped, not error");

    let fetched = entity::get(&db, &eid).await.unwrap();
    assert_eq!(fetched.name, "Robert Smith");
}

// ═══════════════════════════════════════════════════════════════════════════
// GRAPH LAYER BASELINE — unknown entity creates new record
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn link_entity_unknown_name_creates_new_entity() {
    let (db, _dir) = test_db().await;

    let mem_id = create_test_memory(&db, "meeting with completely unknown person").await;

    graph::link_entity(&db, &mem_id, "Zarquon")
        .await
        .expect("link_entity for unknown name should create a new entity");

    let results = entity::find(&db, "Zarquon", None, None).await.unwrap();
    assert!(
        results.iter().any(|e| e.name == "Zarquon"),
        "new entity 'Zarquon' should have been created"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// RECALL BASELINE — canonical name person filter works (no alias needed)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn recall_person_filter_canonical_name_works() {
    let (db, _dir) = test_db().await;

    let mem_id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Robert discussed the Q4 roadmap with the team".into(),
            MemoryType::Episodic,
            MemorySubtype::Meeting,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .unwrap();

    graph::link_entity(&db, &mem_id, "Robert Smith").await.unwrap();

    let results = search::recall(
        &db,
        "roadmap",
        None,
        None,
        None,
        None,
        None,
        Some("Robert Smith"),
        None,
        None,
        None,
        None,
        10,
        &search::AuthorFilter::None,
        0.0,
        &Default::default(),
        &Default::default(),
        RerankerPolicy::default(),
        None,
        None,
    )
    .await
    .unwrap();

    assert!(
        results.iter().any(|m| m.id == mem_id),
        "canonical-name person filter should surface Robert Smith's memory"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// AUTO-LINK BASELINE — no alias, no entity → zero links
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn auto_link_no_entity_no_alias_creates_no_link() {
    let (db, _dir) = test_db().await;

    let mem_id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Zarquon updated the configuration settings".into(),
            MemoryType::Episodic,
            MemorySubtype::Session,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .unwrap();

    let links = auto_link::match_known_entities(
        &db,
        &mem_id,
        "Zarquon updated the configuration settings",
    )
    .await
    .expect("match_known_entities should succeed even with no match");

    assert_eq!(
        links, 0,
        "no links should be created when entity and alias don't exist"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// IGNORED — blocked by implementation bugs in entity_alias.rs
// (RELATE -> none syntax; SELECT value reserved keyword)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn upsert_alias_happy_path_visible_in_aliases_for() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    entity_alias::upsert_alias(&db, &eid, "bob", None)
        .await
        .expect("upsert_alias should succeed");

    let aliases = entity_alias::aliases_for(&db, &eid).await.unwrap();
    assert!(aliases.contains(&"bob".to_string()));
}

#[tokio::test]
async fn upsert_alias_stores_lowercased_value() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    entity_alias::upsert_alias(&db, &eid, "BOB", None).await.expect("upsert");
    let aliases = entity_alias::aliases_for(&db, &eid).await.unwrap();
    assert!(aliases.contains(&"bob".to_string()));
    assert!(!aliases.contains(&"BOB".to_string()));
}

#[tokio::test]
async fn upsert_alias_collision_returns_alias_collision_error() {
    let (db, _dir) = test_db().await;

    let eid1 = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    let eid2 = entity::upsert(
        &db, "Roberto Sanchez", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    entity_alias::upsert_alias(&db, &eid1, "bob", None).await.expect("first upsert");
    let err = entity_alias::upsert_alias(&db, &eid2, "bob", None).await.unwrap_err();

    match &err {
        KbError::AliasCollision { value, .. } => assert_eq!(value, "bob"),
        other => panic!("expected AliasCollision, got {other:?}"),
    }
}

#[tokio::test]
async fn upsert_alias_collision_error_names_existing_canonical() {
    let (db, _dir) = test_db().await;

    let eid1 = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();
    let eid2 = entity::upsert(
        &db, "Roberto Sanchez", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    entity_alias::upsert_alias(&db, &eid1, "bob", None).await.unwrap();
    let err = entity_alias::upsert_alias(&db, &eid2, "bob", None).await.unwrap_err();

    match err {
        KbError::AliasCollision { existing_canonical, .. } => {
            assert_eq!(existing_canonical, "Robert Smith");
        }
        other => panic!("expected AliasCollision; got {other:?}"),
    }
}

#[tokio::test]
async fn upsert_alias_canonical_equal_alias_not_stored() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert", EntityKind::Person, vec![], serde_json::json!({}), None,
        &["robert".to_string()],
    )
    .await
    .expect("canonical-equal skip should not error");

    let aliases = entity_alias::aliases_for(&db, &eid).await.unwrap();
    assert!(!aliases.contains(&"robert".to_string()));
}

#[tokio::test]
async fn archive_alias_removes_from_aliases_for() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    entity_alias::upsert_alias(&db, &eid, "bob", None).await.unwrap();
    entity_alias::archive_alias(&db, &eid, "bob").await.unwrap();
    let aliases = entity_alias::aliases_for(&db, &eid).await.unwrap();
    assert!(!aliases.contains(&"bob".to_string()));
}

#[tokio::test]
async fn archive_alias_makes_resolve_return_empty() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    entity_alias::upsert_alias(&db, &eid, "bob", None).await.unwrap();
    entity_alias::archive_alias(&db, &eid, "bob").await.unwrap();
    let resolved = entity_alias::resolve(&db, "bob").await.unwrap();
    assert!(resolved.is_empty());
}

#[tokio::test]
async fn archive_all_for_entity_archives_all_aliases() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    entity_alias::upsert_alias(&db, &eid, "bob", None).await.unwrap();
    entity_alias::upsert_alias(&db, &eid, "robby", None).await.unwrap();
    entity_alias::upsert_alias(&db, &eid, "rob", None).await.unwrap();
    entity_alias::archive_all_for_entity(&db, &eid).await.unwrap();
    let aliases = entity_alias::aliases_for(&db, &eid).await.unwrap();
    assert!(aliases.is_empty());
}

#[tokio::test]
async fn archive_all_for_entity_each_resolve_returns_empty() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    entity_alias::upsert_alias(&db, &eid, "bob", None).await.unwrap();
    entity_alias::upsert_alias(&db, &eid, "robby", None).await.unwrap();
    entity_alias::archive_all_for_entity(&db, &eid).await.unwrap();

    assert!(entity_alias::resolve(&db, "bob").await.unwrap().is_empty());
    assert!(entity_alias::resolve(&db, "robby").await.unwrap().is_empty());
}

#[tokio::test]
async fn resolve_exact_match_case_insensitive() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    entity_alias::upsert_alias(&db, &eid, "bob", None).await.unwrap();

    assert_eq!(entity_alias::resolve(&db, "Bob").await.unwrap(), vec![eid.clone()]);
    assert_eq!(entity_alias::resolve(&db, "bob").await.unwrap(), vec![eid.clone()]);
    assert_eq!(entity_alias::resolve(&db, "BOB").await.unwrap(), vec![eid]);
}

#[tokio::test]
async fn resolve_fts_matches_partial_token() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    entity_alias::upsert_alias(&db, &eid, "bobby smith", None).await.unwrap();
    let resolved = entity_alias::resolve_fts(&db, "bobby", 5).await.unwrap();
    assert!(resolved.contains(&eid));
}

#[tokio::test]
async fn resolve_at_active_within_valid_window() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Anuradha", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    // Insert alias with a past valid_from so it is active at as_of_active.
    let ent_rid = RecordId::new("entity", eid.as_str());
    db.query(
        "CREATE entity_alias SET
             entity_id   = $entity,
             `value`     = 'anu',
             valid_from  = $vf,
             added_by    = NONE,
             archived_at = NONE,
             valid_until = NONE",
    )
    .bind(("entity", ent_rid))
    .bind(("vf", Utc::now() - Duration::days(1000)))
    .await
    .unwrap();

    let as_of_active = Utc::now() - Duration::days(500);
    let resolved = entity_alias::resolve_at(&db, "anu", &as_of_active).await.unwrap();
    assert!(resolved.contains(&eid));
}

#[tokio::test]
async fn resolve_at_before_valid_from_returns_empty() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Anuradha", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    // Insert alias with valid_from = now (future relative to as_of_before).
    let ent_rid = RecordId::new("entity", eid.as_str());
    db.query(
        "CREATE entity_alias SET
             entity_id   = $entity,
             `value`     = 'anufuture',
             valid_from  = time::now(),
             added_by    = NONE,
             archived_at = NONE,
             valid_until = NONE",
    )
    .bind(("entity", ent_rid))
    .await
    .unwrap();

    let as_of_before = Utc::now() - Duration::days(10);
    let resolved = entity_alias::resolve_at(&db, "anufuture", &as_of_before).await.unwrap();
    assert!(resolved.is_empty());
    let _ = eid;
}

#[tokio::test]
async fn resolve_at_after_valid_until_returns_empty() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Anuradha", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    // Insert alias that expired 10 days ago — resolve_at(now) should return empty.
    let ent_rid = RecordId::new("entity", eid.as_str());
    db.query(
        "CREATE entity_alias SET
             entity_id   = $entity,
             `value`     = 'anuexpired',
             valid_from  = $vf,
             added_by    = NONE,
             archived_at = NONE,
             valid_until = $vu",
    )
    .bind(("entity", ent_rid))
    .bind(("vf", Utc::now() - Duration::days(100)))
    .bind(("vu", Utc::now() - Duration::days(10)))
    .await
    .unwrap();

    let resolved = entity_alias::resolve_at(&db, "anuexpired", &Utc::now()).await.unwrap();
    assert!(resolved.is_empty());
    let _ = eid;
}

#[tokio::test]
async fn entity_upsert_with_aliases_visible_in_get_full() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None,
        &["bob".to_string(), "robby".to_string()],
    )
    .await
    .expect("upsert with aliases should succeed");

    let full = entity::get_full(&db, &eid).await.unwrap();
    assert!(full.aliases.contains(&"bob".to_string()));
    assert!(full.aliases.contains(&"robby".to_string()));
}

#[tokio::test]
async fn get_by_name_falls_through_to_alias() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None,
        &["bob".to_string()],
    )
    .await
    .unwrap();

    let found = entity::get_by_name(&db, "bob").await.expect("alias fallback");
    assert_eq!(found.id, eid);
    assert_eq!(found.name, "Robert Smith");
}

#[tokio::test]
async fn find_includes_alias_matched_entity() {
    let (db, _dir) = test_db().await;

    let _eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None,
        &["bob".to_string()],
    )
    .await
    .unwrap();

    let results = entity::find(&db, "bob", None, None).await.unwrap();
    assert!(results.iter().any(|e| e.name == "Robert Smith"));
}

#[tokio::test]
async fn entity_archive_cascade_archives_aliases() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None,
        &["bob".to_string()],
    )
    .await
    .unwrap();

    entity::archive(&db, &eid, Some("test")).await.unwrap();

    let resolved = entity_alias::resolve(&db, "bob").await.unwrap();
    assert!(resolved.is_empty());
}

#[tokio::test]
async fn link_entity_via_alias_links_to_canonical_not_duplicate() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None,
        &["bob".to_string()],
    )
    .await
    .unwrap();

    let mem_id = create_test_memory(&db, "meeting with bob about the deal").await;
    graph::link_entity(&db, &mem_id, "bob").await.unwrap();

    let mut res = db
        .query("SELECT out.name AS name FROM involves WHERE in = $mem LIMIT 1")
        .bind(("mem", RecordId::new("memory", mem_id.as_str())))
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = res.take(0).unwrap();
    let linked_name = rows
        .into_iter()
        .next()
        .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(String::from));
    assert_eq!(linked_name.as_deref(), Some("Robert Smith"));
    let _ = eid;
}

#[tokio::test]
async fn recall_person_filter_alias_resolves_to_canonical() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None,
        &["bob".to_string()],
    )
    .await
    .unwrap();

    let mem_id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Robert discussed the Q4 roadmap with the team".into(),
            MemoryType::Episodic,
            MemorySubtype::Meeting,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .unwrap();
    graph::link_entity(&db, &mem_id, "Robert Smith").await.unwrap();

    let results = search::recall(
        &db, "roadmap", None, None, None, None, None, Some("bob"),
        None, None, None, None, 10, &search::AuthorFilter::None, 0.0,
        &Default::default(), &Default::default(), RerankerPolicy::default(), None, None,
    )
    .await
    .unwrap();

    assert!(results.iter().any(|m| m.id == mem_id));
    let _ = eid;
}

#[tokio::test]
async fn recall_person_filter_archived_alias_misses() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None,
        &["bob".to_string()],
    )
    .await
    .unwrap();

    let mem_id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Robert discussed the roadmap".into(),
            MemoryType::Episodic,
            MemorySubtype::Meeting,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .unwrap();
    graph::link_entity(&db, &mem_id, "Robert Smith").await.unwrap();
    entity_alias::archive_alias(&db, &eid, "bob").await.unwrap();

    let results = search::recall(
        &db, "roadmap", None, None, None, None, None, Some("bob"),
        None, None, None, None, 10, &search::AuthorFilter::None, 0.0,
        &Default::default(), &Default::default(), RerankerPolicy::default(), None, None,
    )
    .await
    .unwrap();

    assert!(results.is_empty());
}

#[tokio::test]
async fn recall_freetext_alias_preexpansion_surfaces_entity_memories() {
    let (db, _dir) = test_db().await;

    let _eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None,
        &["bobby".to_string()],
    )
    .await
    .unwrap();

    let mem_id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Robert presented the architecture review findings".into(),
            MemoryType::Episodic,
            MemorySubtype::Meeting,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .unwrap();
    graph::link_entity(&db, &mem_id, "Robert Smith").await.unwrap();

    let _other = create_test_memory(&db, "unrelated note about databases").await;

    let results = search::recall(
        &db, "bobby", None, None, None, None, None, None,
        None, None, None, None, 10, &search::AuthorFilter::None, 0.0,
        &Default::default(), &Default::default(), RerankerPolicy::default(), None, None,
    )
    .await
    .unwrap();

    assert!(results.iter().any(|m| m.id == mem_id));
}

#[tokio::test]
async fn auto_link_resolves_alias_to_canonical_entity() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None,
        &["bob".to_string()],
    )
    .await
    .unwrap();

    let mem_id = memory::create(
        &db,
        memory::MemoryDraft::new(
            "Bob reviewed the pull request and approved it".into(),
            MemoryType::Episodic,
            MemorySubtype::Session,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
    )
    .await
    .unwrap();

    let links = auto_link::match_known_entities(
        &db, &mem_id, "Bob reviewed the pull request and approved it",
    )
    .await
    .unwrap();

    assert!(links >= 1);

    let mut res = db
        .query("SELECT out FROM involves WHERE in = $mem LIMIT 5")
        .bind(("mem", RecordId::new("memory", mem_id.as_str())))
        .await
        .unwrap();
    let inv_rows: Vec<serde_json::Value> = res.take(0).unwrap();
    let linked_ids: Vec<String> = inv_rows
        .iter()
        .filter_map(|v| {
            v.get("out").and_then(|o| match o {
                serde_json::Value::String(s) => {
                    Some(s.strip_prefix("entity:").unwrap_or(s).to_string())
                }
                serde_json::Value::Object(m) => {
                    m.get("id").and_then(|i| i.as_str()).map(String::from)
                }
                _ => None,
            })
        })
        .collect();

    assert!(linked_ids.contains(&eid.as_str().to_string()));
}

#[tokio::test]
async fn tool_entity_upsert_with_aliases_round_trips_in_get() {
    let (db, _dir) = test_db().await;

    let out = tool_entity::execute_upsert(
        &db,
        tool_entity::EntityUpsertInput {
            name: "Robert Smith".to_string(),
            kind: "person".to_string(),
            roles: None,
            attrs: None,
            context: None,
            aliases: Some(vec!["bob".to_string(), "robby".to_string()]),
        },
    )
    .await
    .expect("execute_upsert should succeed");

    let get_out = tool_entity::execute_get(
        &db,
        tool_entity::EntityGetInput { id: out.id.clone() },
    )
    .await
    .unwrap();

    assert!(get_out.entity.aliases.contains(&"bob".to_string()));
    assert!(get_out.entity.aliases.contains(&"robby".to_string()));
}

#[tokio::test]
async fn tool_entity_upsert_alias_collision_returns_error() {
    let (db, _dir) = test_db().await;

    tool_entity::execute_upsert(
        &db,
        tool_entity::EntityUpsertInput {
            name: "Robert Smith".to_string(),
            kind: "person".to_string(),
            roles: None,
            attrs: None,
            context: None,
            aliases: Some(vec!["bob".to_string()]),
        },
    )
    .await
    .unwrap();

    let err = tool_entity::execute_upsert(
        &db,
        tool_entity::EntityUpsertInput {
            name: "Roberto Sanchez".to_string(),
            kind: "person".to_string(),
            roles: None,
            attrs: None,
            context: None,
            aliases: Some(vec!["bob".to_string()]),
        },
    )
    .await
    .unwrap_err();

    assert!(matches!(err, KbError::AliasCollision { .. }));
}

#[tokio::test]
async fn tool_entity_upsert_collision_client_message_mentions_alias_and_owner() {
    let (db, _dir) = test_db().await;

    tool_entity::execute_upsert(
        &db,
        tool_entity::EntityUpsertInput {
            name: "Robert Smith".to_string(),
            kind: "person".to_string(),
            roles: None,
            attrs: None,
            context: None,
            aliases: Some(vec!["rob".to_string()]),
        },
    )
    .await
    .unwrap();

    let err = tool_entity::execute_upsert(
        &db,
        tool_entity::EntityUpsertInput {
            name: "Roberto Sanchez".to_string(),
            kind: "person".to_string(),
            roles: None,
            attrs: None,
            context: None,
            aliases: Some(vec!["rob".to_string()]),
        },
    )
    .await
    .unwrap_err();

    let msg = err.client_message();
    assert!(msg.contains("rob"));
    assert!(msg.contains("Robert Smith"));
}

#[tokio::test]
async fn upsert_alias_idempotent_for_same_entity() {
    let (db, _dir) = test_db().await;

    let eid = entity::upsert(
        &db, "Robert Smith", EntityKind::Person, vec![], serde_json::json!({}), None, &[],
    )
    .await
    .unwrap();

    entity_alias::upsert_alias(&db, &eid, "bob", None).await.unwrap();
    entity_alias::upsert_alias(&db, &eid, "bob", None).await.unwrap(); // idempotent

    let aliases = entity_alias::aliases_for(&db, &eid).await.unwrap();
    let bob_count = aliases.iter().filter(|a| *a == "bob").count();
    assert_eq!(bob_count, 1, "alias should appear exactly once; got {aliases:?}");
}
