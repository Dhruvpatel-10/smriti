//! Tests for `reembed_all` — the re-embedding migration tool.
//!
//! `reembed_all` paginates through ALL active memories via LIMIT+OFFSET,
//! calls the embedder for each summary, and stores the resulting vector.

mod common;
use common::{create_test_memory, test_db, MockEmbedder};

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use smriti::config::{EmbeddingConfig, EmbeddingProvider};
use smriti::db::chunking::{ChunkParent, CHUNK_THRESHOLD};
use smriti::db::embeddings::{fingerprint::EmbedderFingerprint, Embedder, LocalEmbedder};
use smriti::db::reembed::reembed_all;
use smriti::db::{chunking, memory};
use smriti::error::Result;
use smriti::types::*;

// ── CountingEmbedder ─────────────────────────────────────────────────────────

/// Wraps another embedder and counts calls to `embed_document_chunks` and
/// `embed_document` separately. Uses `Arc<AtomicUsize>` counters so the test
/// can read counts after the `Arc<dyn Embedder>` has been passed into reembed_all.
struct CountingEmbedder {
    inner: MockEmbedder,
    chunks_calls: Arc<AtomicUsize>,
    document_calls: Arc<AtomicUsize>,
}

impl CountingEmbedder {
    fn new(inner: MockEmbedder) -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let chunks_calls = Arc::new(AtomicUsize::new(0));
        let document_calls = Arc::new(AtomicUsize::new(0));
        let e = Self {
            inner,
            chunks_calls: chunks_calls.clone(),
            document_calls: document_calls.clone(),
        };
        (e, chunks_calls, document_calls)
    }
}

#[async_trait]
impl Embedder for CountingEmbedder {
    fn fingerprint(&self) -> EmbedderFingerprint {
        self.inner.fingerprint()
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }

    fn max_input_tokens(&self) -> usize {
        self.inner.max_input_tokens()
    }

    fn max_chunks_per_doc(&self) -> usize {
        self.inner.max_chunks_per_doc()
    }

    fn recommended_chunk_tokens(&self) -> usize {
        self.inner.recommended_chunk_tokens()
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.inner.embed_query(text).await
    }

    async fn embed_document(&self, text: &str) -> Result<Vec<f32>> {
        self.document_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.embed_document(text).await
    }

    async fn embed_document_chunks(&self, chunks: &[String]) -> Result<Vec<Vec<f32>>> {
        self.chunks_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.embed_document_chunks(chunks).await
    }
}

// ── Large-content parent helper ───────────────────────────────────────────────

fn make_large_chunk_parent() -> ChunkParent {
    let mut content = String::new();
    for i in 0..20 {
        content.push_str(&format!(
            "Section {i}: detailed notes about architecture decisions, trade-offs, \
             SurrealDB design choices, and embedding pipeline configuration. \
             This is filler content to exceed the chunk threshold.\n\n"
        ));
    }
    assert!(content.len() >= CHUNK_THRESHOLD);
    ChunkParent {
        summary: "Large document for sibling-batch reembed test".to_string(),
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

/// Build a Local-backed Embedder configured for the smallest available model.
fn make_embedder() -> Arc<dyn Embedder> {
    let config = EmbeddingConfig {
        enabled: true,
        provider: EmbeddingProvider::Local,
        model: "all-minilm-l6-v2".to_string(),
        dimensions: 384,
        voyage: None,
    };
    Arc::new(LocalEmbedder::new(&config))
}

// ═══════════════════════════════════════════════════════════════════════════
// Basic correctness
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn reembed_all_empty_db_returns_zero() {
    let (db, _dir) = test_db().await;
    let embedder = make_embedder();

    let result = reembed_all(&db, embedder, 32).await.unwrap();

    assert_eq!(result.updated, 0, "empty DB should have 0 updated");
    assert_eq!(result.failed, 0, "empty DB should have 0 failed");
    assert_eq!(result.total, 0, "empty DB should have total=0");
}

#[tokio::test]
async fn reembed_all_total_equals_updated_plus_failed() {
    let (db, _dir) = test_db().await;
    let embedder = make_embedder();

    for i in 0..5usize {
        create_test_memory(&db, &format!("Memory {i}: reembed accounting check")).await;
    }

    let result = reembed_all(&db, embedder, 32).await.unwrap();

    assert_eq!(
        result.total,
        result.updated + result.failed,
        "total must equal updated + failed; got total={}, updated={}, failed={}",
        result.total,
        result.updated,
        result.failed
    );
}

#[tokio::test]
async fn reembed_all_total_equals_memory_count() {
    let (db, _dir) = test_db().await;
    let embedder = make_embedder();

    let count = 7u64;
    for i in 0..count {
        create_test_memory(&db, &format!("Reembed count probe memory {i} unique")).await;
    }

    let result = reembed_all(&db, embedder, 32).await.unwrap();

    assert_eq!(
        result.total, count,
        "reembed_all should process exactly {count} memories (all active), got {}",
        result.total
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Pagination correctness
// ═══════════════════════════════════════════════════════════════════════════

/// The critical pagination correctness test: more memories than one batch.
/// Batch size = 3, memories = 7 → 3 batches of (3, 3, 1).
/// The loop must terminate and visit every record.
#[tokio::test]
async fn reembed_all_batches_correctly_processes_all_memories() {
    let (db, _dir) = test_db().await;
    let embedder = make_embedder();

    let count = 7u64;
    for i in 0..count {
        create_test_memory(&db, &format!("Batch pagination probe memory {i} unique")).await;
    }

    let result = reembed_all(&db, embedder, 3).await.unwrap();

    assert_eq!(
        result.total, count,
        "pagination loop must visit all {count} memories across batches, got total={}",
        result.total
    );
}

/// Exact-boundary: memories == batch_size * N → the loop breaks cleanly after
/// N full batches when the next fetch returns 0 rows.
#[tokio::test]
async fn reembed_all_does_not_double_count_on_exact_batch_boundary() {
    let (db, _dir) = test_db().await;
    let embedder = make_embedder();

    let count = 6u64;
    for i in 0..count {
        create_test_memory(&db, &format!("Exact-boundary memory {i} batch probe unique")).await;
    }

    let result = reembed_all(&db, embedder, 3).await.unwrap();

    assert_eq!(
        result.total, count,
        "exact-boundary batch must not double-count; expected total={count}, got {}",
        result.total
    );
}

/// Archived memories must not appear in the pagination (WHERE status = 'active').
#[tokio::test]
async fn reembed_all_skips_archived_memories() {
    let (db, _dir) = test_db().await;
    let embedder = make_embedder();

    for i in 0..3usize {
        create_test_memory(&db, &format!("Active memory {i} for archive skip test")).await;
    }

    let archived_id =
        create_test_memory(&db, "This memory will be archived before reembed runs").await;
    smriti::db::memory::archive(&db, &archived_id, smriti::types::InvalidationReason::Forgotten)
        .await
        .unwrap();

    let result = reembed_all(&db, embedder, 32).await.unwrap();

    assert_eq!(
        result.total, 3,
        "archived memory must not be processed by reembed_all; expected total=3, got {}",
        result.total
    );
}

/// Degenerate batch size of 1: must still terminate without infinite-looping.
#[tokio::test]
async fn reembed_all_batch_size_one_terminates() {
    let (db, _dir) = test_db().await;
    let embedder = make_embedder();

    let count = 4u64;
    for i in 0..count {
        create_test_memory(&db, &format!("Single-batch memory {i} termination probe")).await;
    }

    let result = reembed_all(&db, embedder, 1).await.unwrap();

    assert_eq!(
        result.total, count,
        "batch_size=1 must still visit all memories; expected total={count}, got {}",
        result.total
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Sibling-batch path (MockEmbedder + CountingEmbedder)
// ═══════════════════════════════════════════════════════════════════════════

/// When the embedder supports contextualized batching (`max_chunks_per_doc > 1`),
/// `reembed_all` must group sibling chunks under the same parent into a single
/// `embed_document_chunks` call — one call per parent group, not one per chunk.
///
/// Setup: 2 parents each with ≥2 chunks → 2 sibling groups.
/// Expected: `embed_document_chunks` called exactly 2 times (one per group),
///           `embed_document` called exactly 2 times (once per parent non-chunk row).
#[tokio::test]
async fn reembed_sibling_batch_counter_accuracy() {
    let (db, _dir) = test_db().await;

    // Use MockEmbedder with contextualized support so max_chunks_per_doc() > 1.
    let mock = MockEmbedder::default_1024().with_contextualized_support(true);
    let (counter_embedder, chunks_count, document_count) = CountingEmbedder::new(mock);
    let embedder: Arc<dyn Embedder> = Arc::new(counter_embedder);

    // Create 2 parents, each with chunks.
    for i in 0..2 {
        let parent = make_large_chunk_parent();
        let parent_id = memory::create(
            &db,
            memory::MemoryDraft::new(
                format!("Parent {i}: {}", parent.summary),
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
        .expect("create parent");

        // chunk_and_embed creates the chunk rows and links them via extends edges.
        // We use a fresh Arc<MockEmbedder> just for the initial chunking — the
        // CountingEmbedder Arc is passed to reembed_all below.
        let chunk_embedder: Arc<dyn Embedder> = Arc::new(
            MockEmbedder::default_1024().with_contextualized_support(true),
        );
        let chunk_ids = chunking::chunk_and_embed(&db, chunk_embedder, &parent_id, &parent)
            .await
            .expect("chunk_and_embed");

        assert!(
            chunk_ids.len() >= 2,
            "parent {i} must produce at least 2 chunks; got {}",
            chunk_ids.len()
        );
    }

    // Reset counters before reembed_all runs (chunk_and_embed calls embedder too
    // but with a different Arc — counters on counting_embedder are still 0).
    assert_eq!(
        chunks_count.load(Ordering::SeqCst),
        0,
        "CountingEmbedder must not have been called yet"
    );

    let result = reembed_all(&db, embedder, 64).await.unwrap();

    // All records (2 parents + N chunks) should complete without failures.
    assert_eq!(
        result.failed, 0,
        "no failures expected; got failed={}",
        result.failed
    );

    // embed_document_chunks must be called exactly once per sibling group (2 parents = 2 groups).
    assert_eq!(
        chunks_count.load(Ordering::SeqCst),
        2,
        "embed_document_chunks must be called once per parent group (2 parents); \
         got {}",
        chunks_count.load(Ordering::SeqCst)
    );

    // embed_document must be called for the 2 parent (non-chunk) rows.
    assert_eq!(
        document_count.load(Ordering::SeqCst),
        2,
        "embed_document must be called once per non-chunk parent row (2 parents); \
         got {}",
        document_count.load(Ordering::SeqCst)
    );
}

/// A chunk row with NULL content must be skipped by `reembed_all`.
/// The `failed` counter must increment by 1 for the skipped chunk,
/// and no vector must be written for that row.
#[tokio::test]
async fn reembed_skips_chunk_with_null_content() {
    let (db, _dir) = test_db().await;

    // MockEmbedder with contextualized support so we exercise the sibling-batch path,
    // which is where the null-content guard lives in reembed.rs.
    let embedder: Arc<dyn Embedder> =
        Arc::new(MockEmbedder::default_1024().with_contextualized_support(true));

    // Create a parent memory.
    let parent_id = create_test_memory(&db, "Parent memory for null-content chunk test").await;

    // Manually insert a chunk memory with NULL content via SurrealQL.
    // Then create an extends edge from the chunk to the parent.
    let chunk_id_str = format!("nullchunk{}", uuid::Uuid::new_v4().simple());
    db.query(
        "CREATE type::record('memory', $chunk_id) CONTENT {
             summary: '[chunk 1/1] null content probe',
             content: NONE,
             memory_type: 'semantic',
             subtype: 'fact',
             channel: 'cli',
             modality: 'text',
             source_ref: NONE,
             project: NONE,
             domain: [],
             people: [],
             importance: 3,
             visibility: 'standard',
             status: 'active',
             tags: [],
             access_count: 0,
             event_at: time::now(),
             created_at: time::now(),
             updated_at: time::now(),
             expires_at: NONE,
             source_agent: NONE,
             session_id: NONE,
             author: NONE,
             valid_until: NONE,
             invalidated_at: NONE,
             invalidation_reason: NONE,
             is_static: false
         }",
    )
    .bind(("chunk_id", chunk_id_str.clone()))
    .await
    .expect("insert null-content chunk");

    // Create the extends edge: chunk → parent.
    let chunk_rid = surrealdb::types::RecordId::new("memory", chunk_id_str.as_str());
    let parent_rid = surrealdb::types::RecordId::new("memory", parent_id.as_str());
    db.query("RELATE $chunk -> extends -> $parent")
        .bind(("chunk", chunk_rid))
        .bind(("parent", parent_rid))
        .await
        .expect("create extends edge");

    // Run reembed_all. The null-content chunk must not be re-embedded (failed += 1).
    // The parent row (non-chunk) will be embedded normally (updated += 1).
    let result = reembed_all(&db, embedder, 64).await.unwrap();

    // failed must include the skipped chunk.
    assert!(
        result.failed >= 1,
        "null-content chunk must increment failed counter; got failed={}",
        result.failed
    );

    // Verify no vector was written for the null-content chunk.
    let mut check = db
        .query("SELECT vector FROM type::record('memory', $id)")
        .bind(("id", chunk_id_str.clone()))
        .await
        .expect("select chunk vector");
    let row: Option<serde_json::Value> = check.take(0).unwrap_or(None);
    let vector_field = row.and_then(|v| v.get("vector").cloned());
    let has_vector = vector_field
        .as_ref()
        .map(|v| !v.is_null())
        .unwrap_or(false);
    assert!(
        !has_vector,
        "null-content chunk must not have a vector written; got: {vector_field:?}"
    );
}
