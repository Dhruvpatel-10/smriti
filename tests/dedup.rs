//! Regression tests for dedup threshold enforcement.
//!
//! The write path enforces two thresholds (src/db/dedup.rs):
//!   REJECT_THRESHOLD = 0.92  — cosine > 0.92 → DedupResult::Duplicate → write rejected
//!   WARN_THRESHOLD   = 0.82  — cosine > 0.82 → DedupResult::Warning   → write succeeds with warning
//!
//! Strategy: test `dedup::check_duplicate` directly by (a) creating a memory at the
//! DB layer and (b) setting its vector via SurrealQL. No real embedder is required.
//! Cosine similarity is therefore fully deterministic.
//!
//! For the reject test (Test 1) we use strategy (c): submit the identical vector as the
//! query — guaranteed cosine = 1.0 > 0.92.
//!
//! For the warn and clean tests we construct orthogonal basis vectors and blend them to
//! hit the desired similarity bracket with arithmetic precision.

mod common;
use common::{create_test_memory, test_db};

use smriti::db::dedup::{self, DedupResult};
use smriti::db::embeddings::store_vector_pub;

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Returns a normalised vector with all weight on dimension `axis` (of `dims` total).
fn unit_vector(dims: usize, axis: usize) -> Vec<f32> {
    let mut v = vec![0.0_f32; dims];
    v[axis] = 1.0;
    v
}

/// Builds a unit vector that is a linear blend of `e0` and `e1`.
///
/// The resulting vector makes angle θ with `e0`, where cos θ = `cos_target`.
/// We solve: v = cos_target * e0 + sin_target * e1, then normalise (already
/// unit-length because e0 and e1 are orthogonal units).
fn blended_unit(dims: usize, cos_target: f32) -> Vec<f32> {
    let sin_target = (1.0_f32 - cos_target * cos_target).sqrt();
    let mut v = vec![0.0_f32; dims];
    v[0] = cos_target;   // e0 component
    v[1] = sin_target;   // e1 component (orthogonal)
    v
}

const DIMS: usize = 256;

// ═══════════════════════════════════════════════════════════════════════════
// Test 1 — Identical vector rejected (cosine = 1.0 > 0.92)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn dedup_identical_vector_returns_duplicate() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "Decided to migrate auth to WebAuthn with passkeys").await;
    let vector = unit_vector(DIMS, 0);
    store_vector_pub(&db, id.as_str(), vector.clone())
        .await
        .expect("failed to store vector");

    // Query with the exact same vector → cosine = 1.0
    let result = dedup::check_duplicate(&db, vector)
        .await
        .expect("check_duplicate failed");

    assert!(
        matches!(result, DedupResult::Duplicate { .. }),
        "identical vector should return Duplicate, got: {result:?}"
    );
}

#[tokio::test]
async fn dedup_identical_vector_has_similarity_above_reject_threshold() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "Passkey migration decision for the auth subsystem").await;
    let vector = unit_vector(DIMS, 0);
    store_vector_pub(&db, id.as_str(), vector.clone())
        .await
        .expect("failed to store vector");

    let result = dedup::check_duplicate(&db, vector)
        .await
        .expect("check_duplicate failed");

    if let DedupResult::Duplicate { similarity, .. } = result {
        assert!(
            similarity > 0.92,
            "duplicate similarity should be > 0.92, got {similarity}"
        );
    } else {
        panic!("expected Duplicate, got: {result:?}");
    }
}

#[tokio::test]
async fn dedup_identical_vector_references_existing_id() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "WebAuthn passkey migration strategy selected").await;
    let vector = unit_vector(DIMS, 0);
    store_vector_pub(&db, id.as_str(), vector.clone())
        .await
        .expect("failed to store vector");

    let result = dedup::check_duplicate(&db, vector)
        .await
        .expect("check_duplicate failed");

    if let DedupResult::Duplicate { existing_id, .. } = result {
        assert!(
            !existing_id.is_empty(),
            "existing_id should be non-empty"
        );
    } else {
        panic!("expected Duplicate, got: {result:?}");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 2 — Near-duplicate (0.82 < cosine ≤ 0.92) returns Warning
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn dedup_near_duplicate_returns_warning() {
    let (db, _dir) = test_db().await;

    // Store the e0 unit vector on the existing memory
    let id = create_test_memory(&db, "Selected PostgreSQL as the primary relational store").await;
    let existing_vector = unit_vector(DIMS, 0);
    store_vector_pub(&db, id.as_str(), existing_vector)
        .await
        .expect("failed to store vector");

    // Query vector: blended so cosine with e0 is 0.87 (between 0.82 and 0.92)
    let query_vector = blended_unit(DIMS, 0.87);

    let result = dedup::check_duplicate(&db, query_vector)
        .await
        .expect("check_duplicate failed");

    assert!(
        matches!(result, DedupResult::Warning { .. }),
        "cosine=0.87 should return Warning, got: {result:?}"
    );
}

#[tokio::test]
async fn dedup_near_duplicate_similarity_is_in_warn_band() {
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "PostgreSQL chosen for relational workload").await;
    let existing_vector = unit_vector(DIMS, 0);
    store_vector_pub(&db, id.as_str(), existing_vector)
        .await
        .expect("failed to store vector");

    let query_vector = blended_unit(DIMS, 0.87);

    let result = dedup::check_duplicate(&db, query_vector)
        .await
        .expect("check_duplicate failed");

    if let DedupResult::Warning { similarity, .. } = result {
        assert!(
            similarity > 0.82,
            "warning similarity should be > 0.82, got {similarity}"
        );
        assert!(
            similarity <= 0.92,
            "warning similarity should be ≤ 0.92, got {similarity}"
        );
    } else {
        panic!("expected Warning, got: {result:?}");
    }
}

#[tokio::test]
async fn dedup_near_duplicate_does_not_prevent_second_write() {
    // At the tool layer (remember::execute) a Warning does NOT abort the write —
    // it appends a note to the message. Verify at the DB layer that check_duplicate
    // returns Warning (not Duplicate), leaving the caller free to proceed.
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "Redis selected for ephemeral session caching").await;
    let existing_vector = unit_vector(DIMS, 0);
    store_vector_pub(&db, id.as_str(), existing_vector)
        .await
        .expect("failed to store vector");

    let query_vector = blended_unit(DIMS, 0.87);

    let result = dedup::check_duplicate(&db, query_vector)
        .await
        .expect("check_duplicate failed");

    // Warning is NOT an error — caller may proceed
    assert!(
        !matches!(result, DedupResult::Duplicate { .. }),
        "near-duplicate should not trigger rejection"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 3 — Dissimilar vectors return Clean (cosine = 0.0 < 0.82)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn dedup_orthogonal_vectors_return_clean() {
    let (db, _dir) = test_db().await;

    // Existing memory has e0
    let id = create_test_memory(&db, "Nginx reverse proxy configuration documented").await;
    let existing_vector = unit_vector(DIMS, 0);
    store_vector_pub(&db, id.as_str(), existing_vector)
        .await
        .expect("failed to store vector");

    // Query with e1 — orthogonal, cosine = 0.0
    let query_vector = unit_vector(DIMS, 1);

    let result = dedup::check_duplicate(&db, query_vector)
        .await
        .expect("check_duplicate failed");

    assert!(
        matches!(result, DedupResult::Clean),
        "orthogonal vectors should return Clean, got: {result:?}"
    );
}

#[tokio::test]
async fn dedup_empty_db_returns_clean() {
    // No memories in DB yet — check_duplicate must return Clean (not panic).
    let (db, _dir) = test_db().await;

    let query_vector = unit_vector(DIMS, 0);

    let result = dedup::check_duplicate(&db, query_vector)
        .await
        .expect("check_duplicate failed on empty DB");

    assert!(
        matches!(result, DedupResult::Clean),
        "empty DB should return Clean, got: {result:?}"
    );
}

#[tokio::test]
async fn dedup_memory_without_vector_is_ignored() {
    // A memory that has no vector (vector = NONE) should not interfere with dedup.
    // The SurrealQL WHERE clause in check_duplicate gates on `vector != NONE`.
    let (db, _dir) = test_db().await;

    // Create a memory but do NOT store a vector on it
    let _id = create_test_memory(&db, "Undocumented note with no vector assigned").await;

    // Query — should be Clean because the existing memory has no vector
    let query_vector = unit_vector(DIMS, 0);
    let result = dedup::check_duplicate(&db, query_vector)
        .await
        .expect("check_duplicate failed");

    assert!(
        matches!(result, DedupResult::Clean),
        "memory with no vector should not trigger dedup, got: {result:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 4 — Dedup is cross-project (no project scope in WHERE clause)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn dedup_rejects_identical_vector_across_different_projects() {
    // src/db/dedup.rs does NOT filter by project — dedup is global across all projects.
    // A duplicate vector in project-b should still be rejected even if the existing
    // memory belongs to project-a.
    let (db, _dir) = test_db().await;

    // Create a memory in project-a
    let id = smriti::db::memory::create(&db,
        smriti::db::memory::MemoryDraft::new("Adopted event sourcing for the orders domain".into(), smriti::types::MemoryType::Semantic, smriti::types::MemorySubtype::Decision, Some(smriti::types::Channel::Cli), Some(smriti::types::Modality::Text))
            .project("project-a".into())
            .importance(smriti::types::Importance::default())
            .visibility(smriti::types::Visibility::default()))
    .await
    .expect("memory create failed");

    let vector = unit_vector(DIMS, 0);
    store_vector_pub(&db, id.as_str(), vector.clone())
        .await
        .expect("failed to store vector");

    // Query with the same vector (simulating a write to project-b)
    let result = dedup::check_duplicate(&db, vector)
        .await
        .expect("check_duplicate failed");

    assert!(
        matches!(result, DedupResult::Duplicate { .. }),
        "identical vector from a different project should still be rejected (dedup is cross-project), got: {result:?}"
    );
}

#[tokio::test]
async fn dedup_archived_memory_does_not_block_new_write() {
    // The dedup WHERE clause requires `status = 'active'`.
    // An archived memory with the same vector should NOT trigger rejection.
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "Archived decision about caching strategy").await;
    let vector = unit_vector(DIMS, 0);
    store_vector_pub(&db, id.as_str(), vector.clone())
        .await
        .expect("failed to store vector");

    // Archive the memory so it is no longer active
    smriti::db::memory::archive(&db, &id, smriti::types::InvalidationReason::Forgotten)
        .await
        .expect("archive failed");

    // Same vector query — the archived memory must be invisible to dedup
    let result = dedup::check_duplicate(&db, vector)
        .await
        .expect("check_duplicate failed");

    assert!(
        matches!(result, DedupResult::Clean),
        "archived memory should not block a new write with the same vector, got: {result:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Test 5 — Boundary: cosine exactly at thresholds
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn dedup_cosine_just_above_reject_threshold_returns_duplicate() {
    // 0.921 > 0.92 — must reject
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "Threshold boundary test: just above reject limit").await;
    let existing_vector = unit_vector(DIMS, 0);
    store_vector_pub(&db, id.as_str(), existing_vector.clone())
        .await
        .expect("failed to store vector");

    let query_vector = blended_unit(DIMS, 0.921);

    let result = dedup::check_duplicate(&db, query_vector)
        .await
        .expect("check_duplicate failed");

    assert!(
        matches!(result, DedupResult::Duplicate { .. }),
        "cosine=0.921 should be rejected (> 0.92 threshold), got: {result:?}"
    );
}

#[tokio::test]
async fn dedup_cosine_just_above_warn_threshold_returns_warning() {
    // 0.83 > 0.82 but ≤ 0.92 — must warn, not reject
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "Threshold boundary test: just above warn limit").await;
    let existing_vector = unit_vector(DIMS, 0);
    store_vector_pub(&db, id.as_str(), existing_vector)
        .await
        .expect("failed to store vector");

    let query_vector = blended_unit(DIMS, 0.83);

    let result = dedup::check_duplicate(&db, query_vector)
        .await
        .expect("check_duplicate failed");

    assert!(
        matches!(result, DedupResult::Warning { .. }),
        "cosine=0.83 should return Warning (> 0.82, ≤ 0.92), got: {result:?}"
    );
}

#[tokio::test]
async fn dedup_cosine_just_below_warn_threshold_returns_clean() {
    // 0.81 < 0.82 — must return Clean
    let (db, _dir) = test_db().await;

    let id = create_test_memory(&db, "Threshold boundary test: just below warn limit").await;
    let existing_vector = unit_vector(DIMS, 0);
    store_vector_pub(&db, id.as_str(), existing_vector)
        .await
        .expect("failed to store vector");

    let query_vector = blended_unit(DIMS, 0.81);

    let result = dedup::check_duplicate(&db, query_vector)
        .await
        .expect("check_duplicate failed");

    assert!(
        matches!(result, DedupResult::Clean),
        "cosine=0.81 should return Clean (< 0.82 warn threshold), got: {result:?}"
    );
}
