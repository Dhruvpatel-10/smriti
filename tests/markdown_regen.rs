//! Tests for `generate_summary` and `regenerate_file` in `src/db/markdown.rs`.
//!
//! `generate_summary` queries with `visibility = 'standard'` throughout —
//! private and sensitive memories must not appear in its output.

mod common;
use common::test_db;

use smriti::config::AlertConfig;
use smriti::db::{markdown, memory};
use smriti::types::*;

fn alert_config() -> AlertConfig {
    AlertConfig::default()
}

// ═══════════════════════════════════════════════════════════════════════════
// generate_summary tests
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn generate_summary_empty_db_contains_knowledge_base_header() {
    let (db, _dir) = test_db().await;

    let md = markdown::generate_summary(&db, &alert_config()).await.unwrap();

    assert!(
        md.contains("# Knowledge Base Summary"),
        "empty DB summary must contain the '# Knowledge Base Summary' header"
    );
}

#[tokio::test]
async fn generate_summary_empty_db_contains_auto_generated_footer() {
    let (db, _dir) = test_db().await;

    let md = markdown::generate_summary(&db, &alert_config()).await.unwrap();

    assert!(
        md.contains("Auto-generated"),
        "empty DB summary must contain the 'Auto-generated' footer"
    );
}

#[tokio::test]
async fn generate_summary_includes_standard_priority_memory() {
    let (db, _dir) = test_db().await;

    let summary_text = "Finish the Q2 roadmap by end of week planning";
    memory::create(&db,
        memory::MemoryDraft::new(summary_text.into(), MemoryType::Working, MemorySubtype::Priority, Some(Channel::Cli), Some(Modality::Text))
            .visibility(Visibility::Standard))
    .await
    .unwrap();

    let md = markdown::generate_summary(&db, &alert_config()).await.unwrap();

    assert!(
        md.contains(summary_text),
        "standard working/priority memory summary should appear in the output"
    );
}

#[tokio::test]
async fn generate_summary_hides_sensitive_priority_memory() {
    let (db, _dir) = test_db().await;

    let secret_text = "Sensitive priority data must not appear in markdown output";
    memory::create(&db,
        memory::MemoryDraft::new(secret_text.into(), MemoryType::Working, MemorySubtype::Priority, Some(Channel::Cli), Some(Modality::Text))
            .visibility(Visibility::Sensitive))
    .await
    .unwrap();

    let md = markdown::generate_summary(&db, &alert_config()).await.unwrap();

    assert!(
        !md.contains(secret_text),
        "sensitive memory summary must NOT appear in markdown output"
    );
}

/// generate_summary has no project scope — this is the cross-context path.
/// A private memory from any project must not leak into the output.
#[tokio::test]
async fn generate_summary_hides_private_memory_in_cross_context() {
    let (db, _dir) = test_db().await;

    let private_text = "Private priority detail that must not appear cross-context";
    memory::create(&db,
        memory::MemoryDraft::new(private_text.into(), MemoryType::Working, MemorySubtype::Priority, Some(Channel::Cli), Some(Modality::Text))
            .project("proj-alpha".into())
            .visibility(Visibility::Private))
    .await
    .unwrap();

    let md = markdown::generate_summary(&db, &alert_config()).await.unwrap();

    assert!(
        !md.contains(private_text),
        "private memory summary must NOT appear in the cross-context markdown output"
    );
}

#[tokio::test]
async fn generate_summary_includes_high_importance_standard_memory() {
    let (db, _dir) = test_db().await;

    let important_text = "High importance decision: adopt event sourcing for orders domain";
    memory::create(&db,
        memory::MemoryDraft::new(important_text.into(), MemoryType::Semantic, MemorySubtype::Decision, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(5).unwrap())
            .visibility(Visibility::Standard))
    .await
    .unwrap();

    let md = markdown::generate_summary(&db, &alert_config()).await.unwrap();

    assert!(
        md.contains(important_text),
        "importance≥4 standard memory should appear in '## Key Memories'"
    );
}

#[tokio::test]
async fn generate_summary_hides_sensitive_high_importance_memory() {
    let (db, _dir) = test_db().await;

    let secret_important = "Sensitive important decision that must be hidden from output";
    memory::create(&db,
        memory::MemoryDraft::new(secret_important.into(), MemoryType::Semantic, MemorySubtype::Decision, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(5).unwrap())
            .visibility(Visibility::Sensitive))
    .await
    .unwrap();

    let md = markdown::generate_summary(&db, &alert_config()).await.unwrap();

    assert!(
        !md.contains(secret_important),
        "sensitive high-importance memory must NOT appear in markdown output"
    );
}

#[tokio::test]
async fn generate_summary_includes_standard_action_item() {
    let (db, _dir) = test_db().await;

    let action_text = "Review the security audit report and file tickets for findings";
    memory::create(&db,
        memory::MemoryDraft::new(action_text.into(), MemoryType::Prospective, MemorySubtype::ActionItem, Some(Channel::Cli), Some(Modality::Text))
            .importance(Importance::new(3).unwrap())
            .visibility(Visibility::Standard))
    .await
    .unwrap();

    let md = markdown::generate_summary(&db, &alert_config()).await.unwrap();

    assert!(
        md.contains(action_text),
        "standard action item should appear in '## Open Action Items'"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// regenerate_file tests
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn regenerate_file_creates_parent_dir_and_writes_file() {
    let (db, _dir) = test_db().await;
    let tmp = tempfile::tempdir().unwrap();

    // Use a nested path whose parent does not yet exist.
    let output_path = tmp.path().join("nested").join("deep").join("CLAUDE.md");
    assert!(
        !output_path.exists(),
        "output file must not exist before regenerate_file"
    );

    markdown::regenerate_file(&db, &alert_config(), &output_path)
        .await
        .unwrap();

    assert!(output_path.exists(), "regenerate_file must create the output file");
}

#[tokio::test]
async fn regenerate_file_output_is_non_empty() {
    let (db, _dir) = test_db().await;
    let tmp = tempfile::tempdir().unwrap();
    let output_path = tmp.path().join("CLAUDE.md");

    markdown::regenerate_file(&db, &alert_config(), &output_path)
        .await
        .unwrap();

    let contents = tokio::fs::read_to_string(&output_path).await.unwrap();
    assert!(
        !contents.is_empty(),
        "regenerate_file must write non-empty content"
    );
}

#[tokio::test]
async fn regenerate_file_output_contains_header() {
    let (db, _dir) = test_db().await;
    let tmp = tempfile::tempdir().unwrap();
    let output_path = tmp.path().join("summary.md");

    markdown::regenerate_file(&db, &alert_config(), &output_path)
        .await
        .unwrap();

    let contents = tokio::fs::read_to_string(&output_path).await.unwrap();
    assert!(
        contents.contains("# Knowledge Base Summary"),
        "written file must contain the knowledge base header"
    );
}

#[tokio::test]
async fn regenerate_file_memory_content_visible_in_written_file() {
    let (db, _dir) = test_db().await;
    let tmp = tempfile::tempdir().unwrap();
    let output_path = tmp.path().join("populated.md");

    let priority_text = "Write tests for all uncovered MCP tools this sprint";
    memory::create(&db,
        memory::MemoryDraft::new(priority_text.into(), MemoryType::Working, MemorySubtype::Priority, Some(Channel::Cli), Some(Modality::Text))
            .visibility(Visibility::Standard))
    .await
    .unwrap();

    markdown::regenerate_file(&db, &alert_config(), &output_path)
        .await
        .unwrap();

    let contents = tokio::fs::read_to_string(&output_path).await.unwrap();
    assert!(
        contents.contains(priority_text),
        "standard working/priority memory must appear in the written file"
    );
}
