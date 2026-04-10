mod common;
use common::{test_db, create_test_memory};

use kb_server::db::memory as db_memory;
use kb_server::types::*;

#[tokio::test]
async fn consolidation_creates_derives_edges() {
    let (db, _dir) = test_db().await;

    let a = create_test_memory(&db, "observation 1").await;
    let b = create_test_memory(&db, "observation 2").await;

    use kb_server::tools::consolidate;
    let out = consolidate::execute(
        &db,
        consolidate::ConsolidateInput {
            ids: vec![a.as_str().to_string(), b.as_str().to_string()],
            result_summary: "combined observation".to_string(),
        },
    )
    .await
    .unwrap();

    // Verify derives edges exist from consolidated -> originals
    let mut result = db
        .query(
            "SELECT record::id(out) AS target FROM derives WHERE in = type::record('memory', $id)",
        )
        .bind(("id", out.id.clone()))
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = result.take(0).unwrap();

    let target_ids: Vec<String> = rows
        .iter()
        .filter_map(|r| r.get("target").and_then(|v| v.as_str()).map(String::from))
        .collect();

    assert!(
        target_ids.contains(&a.as_str().to_string()),
        "derives edge should point to original A"
    );
    assert!(
        target_ids.contains(&b.as_str().to_string()),
        "derives edge should point to original B"
    );

    // Originals should be archived
    let mem_a = db_memory::get(&db, &a).await.unwrap();
    let mem_b = db_memory::get(&db, &b).await.unwrap();
    assert_eq!(mem_a.status, MemoryStatus::Archived);
    assert_eq!(mem_b.status, MemoryStatus::Archived);
}
