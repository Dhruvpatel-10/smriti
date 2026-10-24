//! HNSW vector index on `memory.vector` (B1).
//!
//! The index dimension must match the active embedder, which is only known at
//! startup, so the index is defined there (not in a migration) by [`ensure`].
//! Until it exists — tests, BM25-only runs, or a dimension mismatch awaiting
//! `kb_reembed` — every vector query falls back to the brute-force cosine scan.
//!
//! Callers splice [`candidate_predicate`] into their WHERE clause in place of
//! `vector != NONE`. The KNN form returns the `k` nearest neighbours *before*
//! the other predicates (bitemporal, visibility, author) filter them, so callers
//! over-fetch `k` relative to the rows they need.

use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{info, warn};

use crate::db::client::SurrealClient;

const INDEX_NAME: &str = "idx_memory_vector_hnsw";

static HNSW_READY: AtomicBool = AtomicBool::new(false);

/// Define the HNSW index for `dim`-dimensional vectors if it does not exist.
///
/// A failure (e.g. stored vectors of another dimension after an embedder swap)
/// is logged and leaves the brute-force path active — never fatal.
pub async fn ensure(db: &SurrealClient, dim: usize) {
    // DDL cannot take bind parameters; `dim` is a usize from the embedder, not user input.
    let sql = format!(
        "DEFINE INDEX IF NOT EXISTS {INDEX_NAME} ON TABLE memory FIELDS vector \
         HNSW DIMENSION {dim} DIST COSINE TYPE F32;"
    );
    match db.query(sql).await.and_then(|r| r.check()) {
        Ok(_) => {
            HNSW_READY.store(true, Ordering::Relaxed);
            info!(dim, "HNSW vector index ready");
        }
        Err(e) => {
            HNSW_READY.store(false, Ordering::Relaxed);
            warn!(dim, "HNSW index unavailable, using brute-force vector scan: {e}");
        }
    }
}

/// Drop the index (before re-embedding into a different dimension).
pub async fn remove(db: &SurrealClient) {
    HNSW_READY.store(false, Ordering::Relaxed);
    if let Err(e) = db.query(format!("REMOVE INDEX IF EXISTS {INDEX_NAME} ON TABLE memory;")).await {
        warn!("removing HNSW index failed: {e}");
    }
}

/// WHERE-clause fragment selecting vector candidates for query vector `$qv`:
/// `vector <|k,ef|> $qv` with the index, `vector != NONE` without it.
pub fn candidate_predicate(k: usize) -> String {
    if HNSW_READY.load(Ordering::Relaxed) {
        let k = k.max(1);
        let ef = (k * 2).max(64);
        format!("vector <|{k},{ef}|> $qv")
    } else {
        "vector != NONE".to_string()
    }
}
