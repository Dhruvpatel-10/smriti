mod common;
use common::{test_db, create_test_memory};

use smriti::db::export;

#[tokio::test]
async fn export_and_import_roundtrip() {
    let (db, _dir) = test_db().await;

    let _id1 = create_test_memory(&db, "memory one").await;
    let _id2 = create_test_memory(&db, "memory two").await;

    let export_dir = _dir.path().join("exports");
    let result = export::export_all(&db, &export_dir, false).await.unwrap();
    assert_eq!(result.memory_count, 2);

    // Verify the file exists
    assert!(std::path::Path::new(&result.path).exists());
}

// ═══════════════════════════════════════════════════════════════════════════
// LEGACY SOURCE → CHANNEL/MODALITY BACKFILL ON IMPORT  (ADR-019 / Migration 016)
// ═══════════════════════════════════════════════════════════════════════════

/// Write a minimal JSONL export line that mimics a pre-ADR-019 row: the legacy
/// `source` field is present but `channel` and `modality` are absent.
fn legacy_memory_jsonl(id: &str, source: &str, summary: &str) -> String {
    let now = chrono::Utc::now().to_rfc3339();
    // All required fields for import_memory_row; channel/modality deliberately absent.
    serde_json::json!({
        "entry_type": "memory",
        "data": {
            "id": format!("memory:{id}"),
            "summary": summary,
            "memory_type": "semantic",
            "subtype": "fact",
            "source": source,
            "visibility": "standard",
            "status": "active",
            "importance": 3,
            "people": [],
            "domain": [],
            "tags": [],
            "access_count": 0,
            "event_at": now,
            "created_at": now,
            "updated_at": now
        }
    })
    .to_string()
}

#[tokio::test]
async fn import_legacy_source_slack_backfills_channel_and_modality() {
    let (db, _dir) = test_db().await;

    // Write a JSONL file with a legacy source="slack" row
    let jsonl_path = _dir.path().join("legacy_slack.jsonl");
    let line = legacy_memory_jsonl("legacy-slack-001", "slack", "Slack message about deployment pipeline");
    std::fs::write(&jsonl_path, format!("{line}\n")).unwrap();

    let result = export::import_from(&db, &jsonl_path).await.unwrap();
    assert_eq!(result.memories_imported, 1, "one memory should be imported");
    assert!(result.errors.is_empty(), "no import errors expected: {:?}", result.errors);

    // Read back the stored channel and modality from the DB
    let mut q = db
        .query("SELECT channel, modality FROM type::record('memory', $id)")
        .bind(("id", "legacy-slack-001".to_string()))
        .await
        .unwrap();
    let row: Option<serde_json::Value> = q.take(0).unwrap();
    let row = row.unwrap();

    let channel = row.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let modality = row.get("modality").and_then(|v| v.as_str()).unwrap_or("");

    assert_eq!(channel, "slack", "legacy source=slack must backfill channel=slack");
    assert_eq!(modality, "text",  "legacy source=slack must backfill modality=text");
}

#[tokio::test]
async fn import_legacy_source_voice_backfills_modality_transcript() {
    let (db, _dir) = test_db().await;

    // "meeting" source → no channel backfill, modality=transcript (per migration_016 mapping)
    let jsonl_path = _dir.path().join("legacy_meeting.jsonl");
    let line = legacy_memory_jsonl("legacy-meeting-001", "meeting", "Meeting transcript from weekly sync");
    std::fs::write(&jsonl_path, format!("{line}\n")).unwrap();

    let result = export::import_from(&db, &jsonl_path).await.unwrap();
    assert_eq!(result.memories_imported, 1, "one memory should be imported");
    assert!(result.errors.is_empty(), "no import errors expected: {:?}", result.errors);

    let mut q = db
        .query("SELECT channel, modality FROM type::record('memory', $id)")
        .bind(("id", "legacy-meeting-001".to_string()))
        .await
        .unwrap();
    let row: Option<serde_json::Value> = q.take(0).unwrap();
    let row = row.unwrap();

    // meeting source → channel unknown (no backfill), modality = transcript
    let channel = row.get("channel").and_then(|v| v.as_str());
    let modality = row.get("modality").and_then(|v| v.as_str()).unwrap_or("");

    assert!(channel.is_none(), "legacy source=meeting has no known channel backfill; expected None, got {:?}", channel);
    assert_eq!(modality, "transcript", "legacy source=meeting must backfill modality=transcript");
}
