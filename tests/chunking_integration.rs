//! Integration tests for content chunking.
//!
//! `chunk_and_embed` is the post-create async step that splits long content
//! into sub-memories and links them via `extends` edges back to the parent.
//! Embedding is best-effort (fire-and-forget); this test focuses on the
//! DB-layer guarantees: child memory creation, edge wiring, and metadata
//! inheritance.

mod common;
use common::test_db;

use std::sync::Arc;

use smriti::config::EmbeddingConfig;
use smriti::db::{chunking, embeddings::LocalEmbedder, memory};
use smriti::types::*;

use chrono::Utc;

/// Build a ChunkParent with content.len() >= CHUNK_THRESHOLD.
fn large_parent(project: Option<String>, domain: Vec<String>) -> chunking::ChunkParent {
    // CHUNK_THRESHOLD = 2000; produce 4000 bytes of content with paragraph breaks
    // so `split_into_chunks` can actually split it.
    let mut content = String::new();
    for i in 0..20 {
        content.push_str(&format!(
            "Paragraph {i}: This is detailed content about architecture decisions. \
             The system uses SurrealDB embedded for local-first storage. \
             Each chunk should inherit project and domain from the parent memory.\n\n"
        ));
    }
    assert!(
        content.len() >= chunking::CHUNK_THRESHOLD,
        "precondition: content must exceed CHUNK_THRESHOLD ({} bytes, got {})",
        chunking::CHUNK_THRESHOLD,
        content.len()
    );

    chunking::ChunkParent {
        summary: "Detailed architecture document with chunking enabled".to_string(),
        content,
        memory_type: MemoryType::Semantic,
        subtype: MemorySubtype::Decision,
        channel: Some(Channel::Cli),
        modality: Some(Modality::Text),
        project,
        domain,
        importance: Importance::new(4).unwrap(),
        visibility: Visibility::Standard,
        event_at: Utc::now(),
        tags: vec!["architecture".to_string()],
        expires_at: None,
        source_agent: None,
        session_id: None,
        author: None,
    }
}

/// `chunk_and_embed` must create child memories in the DB and link each
/// with an `extends` edge back to the parent. Children must also inherit
/// project and domain from the parent.
#[tokio::test]
async fn chunk_and_embed_creates_child_memories_with_extends_edges() {
    let (db, _dir) = test_db().await;

    // Create the parent memory.
    let parent = large_parent(Some("infra".to_string()), vec!["database".to_string()]);

    let parent_id = memory::create(&db,
        memory::MemoryDraft::new(parent.summary.clone(), parent.memory_type, parent.subtype, parent.channel, parent.modality)
            .content(parent.content.clone())
            .maybe_project(parent.project.clone())
            .domain(parent.domain.clone())
            .importance(parent.importance)
            .visibility(parent.visibility)
            .event_at(parent.event_at)
            .tags(parent.tags.clone())
            .maybe_expires_at(parent.expires_at)
            .maybe_source_agent(parent.source_agent.clone())
            .maybe_session_id(parent.session_id.clone()))
    .await
    .expect("create parent memory");

    // Build an embedder — embedding will fail silently (model not loaded).
    // That is acceptable: chunk creation and edge linking happen before embed.
    let cfg = EmbeddingConfig::default();
    let embedder = Arc::new(LocalEmbedder::new(&cfg)) as Arc<dyn smriti::db::embeddings::Embedder>;

    let chunk_ids = chunking::chunk_and_embed(&db, embedder, &parent_id, &parent)
        .await
        .expect("chunk_and_embed should succeed");

    // At least 2 chunks must be created (content is ~4k bytes).
    assert!(
        chunk_ids.len() >= 2,
        "expected at least 2 chunks for content > CHUNK_THRESHOLD, got {}",
        chunk_ids.len()
    );

    // Each chunk must exist in the DB and have an `extends` edge to the parent.
    for chunk_id in &chunk_ids {
        // Chunk memory must be retrievable.
        let chunk_mem = memory::get(&db, chunk_id)
            .await
            .unwrap_or_else(|_| panic!("chunk {chunk_id} must exist in DB"));

        // Chunk must inherit project from parent.
        assert_eq!(
            chunk_mem.project.as_deref(),
            Some("infra"),
            "chunk {chunk_id} must inherit project from parent"
        );

        // Chunk must inherit domain from parent.
        assert!(
            chunk_mem.domain.contains(&"database".to_string()),
            "chunk {chunk_id} must inherit domain from parent; got {:?}",
            chunk_mem.domain
        );

        // Verify `extends` edge from chunk to parent.
        let chunk_rid = surrealdb::types::RecordId::new("memory", chunk_id.as_str());
        let parent_rid = surrealdb::types::RecordId::new("memory", parent_id.as_str());
        let mut edge_result = db
            .query("SELECT * FROM extends WHERE in = $chunk AND out = $parent")
            .bind(("chunk", chunk_rid))
            .bind(("parent", parent_rid))
            .await
            .expect("edge query failed");
        let edges: Vec<serde_json::Value> = edge_result.take(0).expect("take edges");
        assert_eq!(
            edges.len(),
            1,
            "chunk {chunk_id} must have exactly one `extends` edge to parent {parent_id}"
        );
    }
}

/// Content shorter than CHUNK_THRESHOLD must produce zero chunks.
#[tokio::test]
async fn chunk_and_embed_short_content_produces_no_chunks() {
    let (db, _dir) = test_db().await;

    let mut parent = large_parent(None, vec![]);
    // Truncate content to below threshold.
    parent.content = "Short content that does not trigger chunking.".to_string();
    assert!(parent.content.len() < chunking::CHUNK_THRESHOLD);

    let parent_id = memory::create(&db,
        memory::MemoryDraft::new(parent.summary.clone(), parent.memory_type, parent.subtype, parent.channel, parent.modality)
            .content(parent.content.clone())
            .maybe_project(parent.project.clone())
            .domain(parent.domain.clone())
            .importance(parent.importance)
            .visibility(parent.visibility)
            .event_at(parent.event_at)
            .tags(parent.tags.clone())
            .maybe_expires_at(parent.expires_at)
            .maybe_source_agent(parent.source_agent.clone())
            .maybe_session_id(parent.session_id.clone()))
    .await
    .expect("create parent memory");

    let cfg = EmbeddingConfig::default();
    let embedder = Arc::new(LocalEmbedder::new(&cfg)) as Arc<dyn smriti::db::embeddings::Embedder>;

    let chunk_ids = chunking::chunk_and_embed(&db, embedder, &parent_id, &parent)
        .await
        .expect("chunk_and_embed should succeed for short content");

    assert!(
        chunk_ids.is_empty(),
        "short content must produce no chunks; got {}",
        chunk_ids.len()
    );
}
