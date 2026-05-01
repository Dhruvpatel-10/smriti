//! Live-API integration tests for the Voyage AI embedder.
//!
//! These tests require a valid `VOYAGE_API_KEY` environment variable.
//! They are marked `#[ignore]` so they are skipped in CI by default.
//! Run them explicitly with:
//!
//!   cargo test --features test-mem -- --ignored
//!
//! or
//!
//!   VOYAGE_API_KEY=... cargo test --features test-mem voyage_integration -- --ignored

use smriti::db::embeddings::{Embedder, VoyageEmbedder};

// ── Cosine similarity helper ──────────────────────────────────────────────────

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "vector dimension mismatch");
    a.iter().zip(b.iter()).map(|(&x, &y)| x * y).sum()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Verify that `embed_query` and `embed_document_chunks` produce vectors in the
/// same embedding space for Voyage voyage-context-3.
///
/// ADR-020 §2 says query and document endpoints share the same output space
/// (asymmetric only in terms of input-type bias, not dimensionality or basis).
/// This test asserts cosine >= 0.85 for identical text through both paths as a
/// sanity check that the two endpoints are compatible for retrieval.
///
/// Skipped if `VOYAGE_API_KEY` is not set.
#[tokio::test]
#[ignore = "requires VOYAGE_API_KEY env var — run with cargo test -- --ignored"]
async fn vector_space_query_vs_document_chunks_same_space() {
    let api_key = match std::env::var("VOYAGE_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            eprintln!("VOYAGE_API_KEY not set — skipping voyage live-API test");
            return;
        }
    };

    let embedder = VoyageEmbedder::new(
        api_key,
        "voyage-context-3".to_string(),
        1024,
        30_000, // 30s timeout
    )
    .expect("build VoyageEmbedder");

    let text = "the quick brown fox";

    let query_vec = embedder
        .embed_query(text)
        .await
        .expect("embed_query must succeed");

    let chunk_vecs = embedder
        .embed_document_chunks(&[text.to_string()])
        .await
        .expect("embed_document_chunks must succeed");

    assert_eq!(chunk_vecs.len(), 1, "one chunk in, one vector out");
    let doc_vec = &chunk_vecs[0];

    assert_eq!(
        query_vec.len(),
        doc_vec.len(),
        "query and document vectors must have same dimensionality"
    );

    let sim = cosine(&query_vec, doc_vec);
    assert!(
        sim >= 0.85,
        "query and document-chunk vectors for identical text must have cosine >= 0.85; \
         got {sim:.4}. This would indicate the two endpoints are in different vector spaces."
    );
}
