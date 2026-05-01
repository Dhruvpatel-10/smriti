/// Shared test helpers used across all integration test suites.
///
/// Not every test binary uses every helper — allow dead code to avoid
/// per-binary unused warnings (see ADR-007: modular test architecture).
pub mod mock_embedder;
#[allow(unused_imports)]
pub use mock_embedder::{cosine, MockEmbedder};

#[allow(unused_imports)]
use smriti::db::memory as db_memory;
#[allow(unused_imports)]
use smriti::tools::remember;

use smriti::db::client;
use smriti::types::*;

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
        db_memory::MemoryDraft::new(
            summary.to_string(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli),
            Some(Modality::Text),
        ),
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
