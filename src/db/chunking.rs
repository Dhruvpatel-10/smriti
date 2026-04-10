//! Content chunking for long documents.
//!
//! When a memory has long `content` (e.g., meeting transcripts, article notes),
//! a single embedding cannot represent all the information well. This module
//! splits content into overlapping chunks, creating sub-memories linked via
//! `extends` edges back to the parent.
//!
//! Each chunk gets its own vector embedding, enabling fine-grained vector search
//! into long documents. The parent memory retains its own embedding (from the
//! summary), so both coarse and fine-grained search paths work.

use std::sync::Arc;

use tracing::{info, instrument, warn};

use crate::db::client::SurrealClient;
use crate::db::embeddings::{spawn_embed_and_store, LocalEmbedder};
use crate::db::graph;
use crate::db::memory;
use crate::error::Result;
use crate::types::*;

/// Default chunk size in characters.
const CHUNK_SIZE: usize = 1500;
/// Overlap between consecutive chunks to preserve context at boundaries.
const CHUNK_OVERLAP: usize = 200;
/// Minimum content length to trigger chunking.
pub const CHUNK_THRESHOLD: usize = 2000;

/// Split text into overlapping chunks, respecting paragraph boundaries.
pub fn split_into_chunks(text: &str, chunk_size: usize, overlap: usize) -> Vec<String> {
    if text.len() <= chunk_size {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let end = (start + chunk_size).min(text.len());

        // Try to break at a paragraph boundary (double newline)
        let actual_end = if end < text.len() {
            text[start..end]
                .rfind("\n\n")
                .map(|pos| start + pos + 2)
                .unwrap_or_else(|| {
                    // Fall back to sentence boundary (period + space)
                    text[start..end]
                        .rfind(". ")
                        .map(|pos| start + pos + 2)
                        .unwrap_or(end)
                })
        } else {
            end
        };

        chunks.push(text[start..actual_end].to_string());

        // Advance by (chunk_end - overlap), ensuring forward progress
        let advance = (actual_end - start).saturating_sub(overlap).max(1);
        start += advance;
    }

    chunks
}

/// For a memory with long content, create chunk sub-memories linked
/// via `extends` edges back to the parent.
///
/// Returns the IDs of created chunk memories. Returns empty vec if
/// content is too short to chunk.
#[instrument(skip(db, embedder, parent), fields(parent_id = %parent_id))]
pub async fn chunk_and_embed(
    db: &SurrealClient,
    embedder: Arc<LocalEmbedder>,
    parent_id: &MemoryId,
    parent: &Memory,
) -> Result<Vec<MemoryId>> {
    let content = match &parent.content {
        Some(c) if c.len() >= CHUNK_THRESHOLD => c,
        _ => return Ok(vec![]),
    };

    let chunks = split_into_chunks(content, CHUNK_SIZE, CHUNK_OVERLAP);
    if chunks.len() <= 1 {
        return Ok(vec![]);
    }

    let total = chunks.len();
    let mut chunk_ids = Vec::with_capacity(total);

    for (i, chunk_text) in chunks.iter().enumerate() {
        let chunk_summary = format!(
            "[chunk {}/{}] {}",
            i + 1,
            total,
            truncate_str(&parent.summary, 80)
        );

        let chunk_id = memory::create(
            db,
            chunk_summary.clone(),
            Some(chunk_text.clone()),
            parent.memory_type,
            parent.subtype,
            parent.source.clone(),
            parent.project.clone(),
            parent.domain.clone(),
            vec![],
            parent.importance,
            parent.visibility,
            Some(parent.event_at),
            parent.tags.clone(),
            parent.expires_at,
            parent.source_agent.clone(),
            parent.session_id.clone(),
        )
        .await?;

        if let Err(e) = graph::link_memories(db, &chunk_id, parent_id, RelationType::Extends).await
        {
            warn!(chunk = i + 1, "failed to link chunk to parent: {e}");
        }

        spawn_embed_and_store(
            embedder.clone(),
            db.clone(),
            chunk_id.as_str().to_string(),
            chunk_text.clone(),
        );

        chunk_ids.push(chunk_id);
    }

    info!(parent = %parent_id, chunks = total, "created content chunks");
    Ok(chunk_ids)
}

fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s.floor_char_boundary(max)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_not_chunked() {
        let chunks = split_into_chunks("hello world", CHUNK_SIZE, CHUNK_OVERLAP);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello world");
    }

    #[test]
    fn long_text_produces_multiple_chunks() {
        // Create text that's about 3x chunk size
        let text = "a".repeat(CHUNK_SIZE * 3);
        let chunks = split_into_chunks(&text, CHUNK_SIZE, CHUNK_OVERLAP);
        assert!(
            chunks.len() >= 3,
            "expected at least 3 chunks, got {}",
            chunks.len()
        );

        // Each chunk should be at most chunk_size long
        for (i, chunk) in chunks.iter().enumerate() {
            assert!(
                chunk.len() <= CHUNK_SIZE + 10, // small tolerance
                "chunk {} is too large: {} bytes",
                i,
                chunk.len()
            );
        }

        // All text should be covered (concatenation minus overlaps >= original)
        let total_chars: usize = chunks.iter().map(|c| c.len()).sum();
        assert!(
            total_chars >= text.len(),
            "chunks should cover all text: {} < {}",
            total_chars,
            text.len()
        );
    }

    #[test]
    fn prefers_paragraph_boundaries() {
        let mut text = String::new();
        // First paragraph: 1000 chars
        text.push_str(&"x".repeat(1000));
        text.push_str("\n\n");
        // Second paragraph: 1000 chars
        text.push_str(&"y".repeat(1000));
        text.push_str("\n\n");
        // Third paragraph: 500 chars
        text.push_str(&"z".repeat(500));

        let chunks = split_into_chunks(&text, CHUNK_SIZE, CHUNK_OVERLAP);
        // First chunk should end at the paragraph boundary
        assert!(
            chunks[0].ends_with("\n\n"),
            "first chunk should end at paragraph boundary"
        );
    }
}
