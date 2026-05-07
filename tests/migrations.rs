mod common;
use common::test_db;

#[tokio::test]
async fn migrations_are_recorded_and_idempotent() {
    let (db, _dir) = test_db().await;
    use smriti::db::migrations;

    // Migrations already ran during connect(). Running again should be no-op.
    migrations::run(&db).await.unwrap();

    // Check that all migrations are recorded
    let mut result = db
        .query("SELECT version FROM _migration ORDER BY version ASC")
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = result.take(0).unwrap();

    let versions: Vec<u32> = rows
        .iter()
        .filter_map(|r| r.get("version").and_then(|v| v.as_u64()).map(|v| v as u32))
        .collect();

    assert_eq!(
        versions,
        vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21],
        "all twenty-one migrations should be recorded"
    );
    assert_eq!(migrations::latest_version(), 21);
}

/// Verify that migration 17 created memory_analyzer_v2 and re-bound the three
/// FT indexes to it. SurrealDB INFO queries return the analyzer/index definitions
/// as strings, so we check that the new analyzer name appears in each index row.
#[tokio::test]
async fn migration_017_analyzer_v2_and_indexes() {
    let (db, _dir) = test_db().await;

    // Confirm memory_analyzer_v2 exists by doing a round-trip FTS query —
    // if the analyzer didn't exist, defining or using the index would have
    // errored during migrations::run inside connect().
    //
    // Use INFO FOR TABLE to inspect the index definitions on the memory table.
    let mut info = db
        .query("INFO FOR TABLE memory")
        .await
        .expect("INFO FOR TABLE memory");

    let table_info: Option<serde_json::Value> = info.take(0).expect("take info result");
    let table_info = table_info.expect("INFO FOR TABLE returned no rows");

    let indexes = table_info
        .get("indexes")
        .expect("indexes key in table info");

    // Each index entry value should be a string containing the DEFINE INDEX statement.
    let summary_def = indexes
        .get("idx_memory_summary_ft")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let content_def = indexes
        .get("idx_memory_content_ft")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let anchor_def = indexes
        .get("idx_memory_anchor_ft")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    assert!(
        summary_def.contains("memory_analyzer_v2"),
        "idx_memory_summary_ft should reference memory_analyzer_v2; got: {summary_def:?}"
    );
    assert!(
        content_def.contains("memory_analyzer_v2"),
        "idx_memory_content_ft should reference memory_analyzer_v2; got: {content_def:?}"
    );
    assert!(
        anchor_def.contains("memory_analyzer_v2"),
        "idx_memory_anchor_ft should reference memory_analyzer_v2; got: {anchor_def:?}"
    );

    // entity_name_analyzer must remain untouched (no ascii filter, class tokenizer only).
    let mut entity_info = db
        .query("INFO FOR TABLE entity")
        .await
        .expect("INFO FOR TABLE entity");
    let entity_table: Option<serde_json::Value> = entity_info.take(0).expect("take entity info");
    let entity_table = entity_table.expect("INFO FOR TABLE entity returned no rows");
    let entity_indexes = entity_table.get("indexes").expect("indexes key on entity");
    let entity_name_ft = entity_indexes
        .get("idx_entity_name_ft")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        entity_name_ft.contains("entity_name_analyzer"),
        "idx_entity_name_ft must still reference entity_name_analyzer; got: {entity_name_ft:?}"
    );
    assert!(
        !entity_name_ft.contains("memory_analyzer_v2"),
        "entity FT index must NOT reference memory_analyzer_v2"
    );
}
