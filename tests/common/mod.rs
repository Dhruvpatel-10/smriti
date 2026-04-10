/// Shared test helpers used across all integration test suites.
///
/// Not every test binary uses every helper — allow dead code to avoid
/// per-binary unused warnings (see ADR-007: modular test architecture).
#[allow(unused_imports)]
use kb_server::db::memory as db_memory;
#[allow(unused_imports)]
use kb_server::tools::remember;

use kb_server::db::client;
use kb_server::types::*;

/// Spin up a fresh embedded DB in a temp directory.
///
/// Returns the client and the TempDir guard — keep the guard alive for the
/// duration of the test or the directory will be cleaned up early.
#[allow(dead_code)]
pub async fn test_db() -> (client::SurrealClient, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let db = client::connect(dir.path()).await.expect("failed to connect");
    (db, dir)
}

/// Create a minimal test memory (Semantic/Fact) at the DB layer.
#[allow(dead_code)]
pub async fn create_test_memory(db: &client::SurrealClient, summary: &str) -> MemoryId {
    db_memory::create(
        db,
        summary.to_string(),
        None,
        MemoryType::Semantic,
        MemorySubtype::Fact,
        Source::Manual,
        None,
        vec![],
        vec![],
        Importance::default(),
        Visibility::default(),
        None,
        vec![],
        None,
        None,
        None,
    )
    .await
    .expect("test memory creation failed")
}

/// Build a minimal RememberInput for tool-level tests.
/// Pads short summaries to satisfy the 10-char validation gate.
#[allow(dead_code)]
pub fn remember_input(
    summary: &str,
    typ: MemoryType,
    sub: MemorySubtype,
) -> remember::RememberInput {
    let padded = if summary.len() < 10 {
        format!("{summary} (test memory data)")
    } else {
        summary.to_string()
    };
    remember::RememberInput {
        summary: padded,
        memory_type: typ,
        subtype: sub,
        source: Source::Manual,
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
    }
}
