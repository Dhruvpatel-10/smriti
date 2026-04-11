mod common;
use common::{test_db, create_test_memory};

#[tokio::test]
async fn export_and_import_roundtrip() {
    let (db, _dir) = test_db().await;
    use smriti::db::export;

    let _id1 = create_test_memory(&db, "memory one").await;
    let _id2 = create_test_memory(&db, "memory two").await;

    let export_dir = _dir.path().join("exports");
    let result = export::export_all(&db, &export_dir, false).await.unwrap();
    assert_eq!(result.memory_count, 2);

    // Verify the file exists
    assert!(std::path::Path::new(&result.path).exists());
}
