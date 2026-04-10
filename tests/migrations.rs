mod common;
use common::test_db;

#[tokio::test]
async fn migrations_are_recorded_and_idempotent() {
    let (db, _dir) = test_db().await;
    use kb_server::db::migrations;

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
        vec![1, 2, 3, 4],
        "all four migrations should be recorded"
    );
    assert_eq!(migrations::latest_version(), 4);
}
