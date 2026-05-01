//! Integration tests for the embedder fingerprint DB gate.
//!
//! These tests verify that `check_or_initialize` correctly enforces fingerprint
//! consistency across restarts, and that `SMRITI_FORCE_REEMBED_ON_START` provides
//! an escape hatch that overwrites the stored fingerprint.
//!
//! WARNING: The env-var tests manipulate `SMRITI_FORCE_REEMBED_ON_START` in the
//! same process. Each test sets and then removes the var within a single `#[tokio::test]`
//! function to avoid leaking across test boundaries. Because the default Rust test
//! harness runs these in separate threads, only the var-setting test needs to be
//! careful — the `unsafe` set/remove is confined to the one test that needs it,
//! and the assertion runs in the same thread before `remove_var`.

mod common;
use common::{test_db, MockEmbedder};

use smriti::db::embeddings::fingerprint::{check_or_initialize, read, write, EmbedderFingerprint};
use smriti::db::embeddings::Embedder;
use smriti::db::embeddings::store_vector_pub;
use smriti::db::migrations;

/// Spin up a test DB with migrations applied — required for `_kb_metadata`
/// table existence and the memory schema used by the vector-store path.
async fn test_db_migrated() -> (smriti::db::client::SurrealClient, tempfile::TempDir) {
    let (db, dir) = test_db().await;
    migrations::run(&db).await.expect("migrations must succeed");
    (db, dir)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Serializes tests that read or mutate `SMRITI_FORCE_REEMBED_ON_START`.
/// Without this, parallel test execution races: one test sets the var while
/// another reads no-var-set and gets a wrong-branch result. The lock makes the
/// set/check/remove sequence observably atomic from each test's perspective.
///
/// `tokio::sync::Mutex` (not `std::sync::Mutex`) — the guard is held across
/// `.await` points, which clippy's `await_holding_lock` rightly forbids for
/// std mutexes.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fingerprint(provider: &str, model: &str, dim: usize) -> EmbedderFingerprint {
    EmbedderFingerprint {
        provider: provider.to_string(),
        model: model.to_string(),
        dimensions: dim,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Fresh DB with no stored fingerprint: `check_or_initialize` must succeed and
/// persist the new fingerprint.
#[tokio::test]
async fn fingerprint_initialize_on_empty_db() {
    let (db, _dir) = test_db_migrated().await;
    let fp = fingerprint("mock", "v1", 1024);

    let result = check_or_initialize(&db, &fp).await;
    assert!(result.is_ok(), "fresh DB must initialize fingerprint without error");

    let stored = read(&db).await.expect("read fingerprint");
    assert_eq!(
        stored,
        Some(fp),
        "stored fingerprint must match the one passed to check_or_initialize"
    );
}

/// Matching fingerprint on second call: must succeed without modifying the DB.
#[tokio::test]
async fn fingerprint_matching_fingerprint_ok() {
    let (db, _dir) = test_db_migrated().await;
    let fp = fingerprint("mock", "v1", 1024);

    // First call stores it.
    check_or_initialize(&db, &fp).await.expect("first init");
    // Second call with identical fingerprint must also succeed.
    let result = check_or_initialize(&db, &fp).await;
    assert!(result.is_ok(), "matching fingerprint on second call must succeed");
}

/// Mismatch with no stored vectors: must silently update fingerprint (no error).
/// The previous fingerprint had no vectors, so there's nothing to invalidate.
#[tokio::test]
async fn fingerprint_mismatch_no_vectors_updates_silently() {
    let (db, _dir) = test_db_migrated().await;
    let fp_v1 = fingerprint("mock", "v1", 1024);
    let fp_v2 = fingerprint("mock", "v2", 1024);

    // Store v1 fingerprint but insert NO vectors.
    write(&db, &fp_v1).await.expect("write v1");

    // v2 mismatches but no vectors exist — must succeed and update.
    let result = check_or_initialize(&db, &fp_v2).await;
    assert!(
        result.is_ok(),
        "mismatch with no vectors must succeed; got: {:?}",
        result
    );

    let stored = read(&db).await.expect("read after update").expect("should be set");
    assert_eq!(stored, fp_v2, "fingerprint must be updated to v2");
}

/// Mismatch WITH stored vectors and WITHOUT the env var: must return an error
/// whose message contains "kb_reembed" and "SMRITI_FORCE_REEMBED_ON_START".
#[tokio::test]
async fn fingerprint_mismatch_with_vectors_returns_error() {
    // Hold the env-var lock for the whole test — the bypass test sets the
    // same var concurrently. Poisoned guard is fine: prior poisoning means a
    // sibling test panicked, the var state is whatever they left it.
    let _guard = ENV_LOCK.lock().await;

    let (db, _dir) = test_db_migrated().await;
    let fp_v1 = fingerprint("mock", "v1", 1024);
    let fp_v2 = fingerprint("mock", "v2", 1024);

    // Ensure the env var is NOT set for this test.
    #[allow(unsafe_code)]
    unsafe {
        std::env::remove_var("SMRITI_FORCE_REEMBED_ON_START");
    }

    // Store v1 fingerprint.
    write(&db, &fp_v1).await.expect("write v1");

    // Insert a fake vector so `count_vectors` returns > 0.
    let mock = MockEmbedder::default_1024();
    let mem_id = smriti::db::memory::create(
        &db,
        smriti::db::memory::MemoryDraft::new(
            "memory with vector for fingerprint test".to_string(),
            smriti::types::MemoryType::Semantic,
            smriti::types::MemorySubtype::Fact,
            Some(smriti::types::Channel::Cli),
            Some(smriti::types::Modality::Text),
        ),
    )
    .await
    .expect("create memory");

    let vector = mock.embed_document("test vector content").await.expect("embed");
    store_vector_pub(&db, mem_id.as_str(), vector)
        .await
        .expect("store vector");

    // Attempt check with mismatched fingerprint — must fail.
    let err = check_or_initialize(&db, &fp_v2)
        .await
        .expect_err("mismatch with vectors must return error");

    let msg = err.to_string();
    assert!(
        msg.contains("kb_reembed"),
        "error must mention kb_reembed; got: {msg}"
    );
    assert!(
        msg.contains("SMRITI_FORCE_REEMBED_ON_START"),
        "error must mention SMRITI_FORCE_REEMBED_ON_START; got: {msg}"
    );
}

/// Mismatch WITH stored vectors AND WITH the env var set: must succeed and
/// update the stored fingerprint to the new value.
///
/// This test sets and then removes `SMRITI_FORCE_REEMBED_ON_START` within a
/// single test function to avoid leaking the env var to sibling tests.
/// `serial_test` is not required because the set/remove is wrapped in a
/// single async future that runs to completion before any other test's
/// `check_or_initialize` would observe the var.
///
/// NOTE: this test uses `unsafe { std::env::set_var }` — required because
/// Rust's `std::env::set_var` is documented as unsafe in Edition 2024 when
/// other threads may be reading environment variables. Tests are isolated
/// enough that this is acceptable.
#[tokio::test]
async fn fingerprint_env_var_bypass_overwrites_stored_value() {
    // Serialize with the no-var sibling test — they manipulate the same env var.
    let _guard = ENV_LOCK.lock().await;

    let (db, _dir) = test_db_migrated().await;
    let fp_v1 = fingerprint("mock", "v1", 1024);
    let fp_v2 = fingerprint("mock", "v2", 1024);

    // Store v1 fingerprint.
    write(&db, &fp_v1).await.expect("write v1");

    // Insert a fake vector so `count_vectors` returns > 0.
    let mock = MockEmbedder::default_1024();
    let mem_id = smriti::db::memory::create(
        &db,
        smriti::db::memory::MemoryDraft::new(
            "memory with vector for bypass test".to_string(),
            smriti::types::MemoryType::Semantic,
            smriti::types::MemorySubtype::Fact,
            Some(smriti::types::Channel::Cli),
            Some(smriti::types::Modality::Text),
        ),
    )
    .await
    .expect("create memory");

    let vector = mock.embed_document("bypass env var test").await.expect("embed");
    store_vector_pub(&db, mem_id.as_str(), vector)
        .await
        .expect("store vector");

    // Verify it fails WITHOUT the env var (baseline).
    #[allow(unsafe_code)]
    unsafe {
        std::env::remove_var("SMRITI_FORCE_REEMBED_ON_START");
    }
    assert!(
        check_or_initialize(&db, &fp_v2).await.is_err(),
        "baseline: must fail without env var"
    );

    // Now set the env var and retry — must succeed.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("SMRITI_FORCE_REEMBED_ON_START", "1");
    }

    let result = check_or_initialize(&db, &fp_v2).await;

    // Always clean up the env var regardless of result.
    #[allow(unsafe_code)]
    unsafe {
        std::env::remove_var("SMRITI_FORCE_REEMBED_ON_START");
    }

    assert!(
        result.is_ok(),
        "SMRITI_FORCE_REEMBED_ON_START=1 must bypass mismatch check; got: {:?}",
        result
    );

    // Stored fingerprint must be updated to v2.
    let stored = read(&db).await.expect("read fingerprint").expect("must be set");
    assert_eq!(
        stored, fp_v2,
        "stored fingerprint must be updated to v2 after bypass"
    );
}
