//! Regression guard: chunk vectors must have the correct dimensionality.
//!
//! Ensures that after `chunk_and_embed` completes, every chunk row in the DB
//! has a stored vector of exactly `embedder.dimensions()` length. This guards
//! against any re-introduction of the deleted `spawn_embed_and_store` pattern
//! (which could write vectors asynchronously after the synchronous return,
//! causing race conditions and potential dimension mismatches).

mod common;
use common::{test_db, MockEmbedder};

use std::sync::Arc;

use chrono::Utc;
use smriti::db::chunking::{self, ChunkParent, CHUNK_THRESHOLD};
use smriti::db::embeddings::Embedder;
use smriti::db::memory;
use smriti::types::*;

const MOCK_DIM: usize = 1024;

/// Build a `ChunkParent` with content exceeding `CHUNK_THRESHOLD` (≥3000 chars).
fn make_large_parent() -> ChunkParent {
    let mut content = String::new();
    for i in 0..25 {
        content.push_str(&format!(
            "Section {i}: Detailed architectural notes on SurrealDB embedding pipeline, \
             chunking strategy, vector dimension correctness, and regression prevention \
             for the spawn_embed_and_store deletion described in ADR-020.\n\n"
        ));
    }
    assert!(
        content.len() >= 3000,
        "content must be at least 3000 chars; got {}",
        content.len()
    );
    assert!(
        content.len() >= CHUNK_THRESHOLD,
        "content must exceed CHUNK_THRESHOLD"
    );

    ChunkParent {
        summary: "Large document for vector dimension regression test".to_string(),
        content,
        memory_type: MemoryType::Semantic,
        subtype: MemorySubtype::Decision,
        channel: Some(Channel::Cli),
        modality: Some(Modality::Text),
        project: None,
        domain: vec![],
        importance: Importance::new(4).unwrap(),
        visibility: Visibility::Standard,
        event_at: Utc::now(),
        tags: vec![],
        expires_at: None,
        source_agent: None,
        session_id: None,
        author: None,
    }
}

/// After `chunk_and_embed` returns synchronously, every chunk DB row must have
/// a vector of exactly `MOCK_DIM` dimensions.
///
/// This is a regression guard against `spawn_embed_and_store` re-introduction:
/// if embedding were async/fire-and-forget, vectors might be written after
/// the function returns — or not at all — producing either missing or
/// wrong-dimension vectors.
#[tokio::test]
async fn chunk_vectors_have_correct_dimensions_no_race() {
    let (db, _dir) = test_db().await;

    let parent = make_large_parent();

    let parent_id = memory::create(
        &db,
        memory::MemoryDraft::new(
            parent.summary.clone(),
            parent.memory_type,
            parent.subtype,
            parent.channel,
            parent.modality,
        )
        .content(parent.content.clone())
        .importance(parent.importance)
        .visibility(parent.visibility)
        .event_at(parent.event_at),
    )
    .await
    .expect("create parent memory");

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new("mock/v1/1024"));

    let chunk_ids = chunking::chunk_and_embed(&db, embedder, &parent_id, &parent)
        .await
        .expect("chunk_and_embed must succeed");

    assert!(
        chunk_ids.len() >= 2,
        "need at least 2 chunks for this test; got {} (content len={})",
        chunk_ids.len(),
        parent.content.len()
    );

    // After chunk_and_embed returns, vectors must already be written synchronously.
    for chunk_id in &chunk_ids {
        let mut result = db
            .query("SELECT vector FROM type::record('memory', $id)")
            .bind(("id", chunk_id.as_str().to_string()))
            .await
            .expect("select chunk vector");

        let row: Option<serde_json::Value> = result.take(0).expect("take");
        let row = row.unwrap_or_else(|| panic!("chunk {chunk_id} row must exist in DB"));

        let vector_val = row.get("vector").unwrap_or_else(|| {
            panic!("chunk {chunk_id} must have a vector field; row={row:?}")
        });

        assert!(
            !vector_val.is_null(),
            "chunk {chunk_id} vector must not be NULL — synchronous write ensures \
             the vector is present immediately after chunk_and_embed returns"
        );

        let vector: Vec<f32> = serde_json::from_value(vector_val.clone())
            .unwrap_or_else(|e| panic!("chunk {chunk_id} vector must deserialize as Vec<f32>: {e}"));

        assert_eq!(
            vector.len(),
            MOCK_DIM,
            "chunk {chunk_id} vector must have exactly {MOCK_DIM} dimensions; \
             got {} dimensions. A wrong-dimension vector would indicate a stale \
             async write from the deleted spawn_embed_and_store path.",
            vector.len()
        );
    }
}
