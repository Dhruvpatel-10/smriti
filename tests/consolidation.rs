mod common;
use common::{test_db, create_test_memory};

use smriti::db::consolidation::{fetch_memories_by_ids, consolidate_atomic, ConsolidateParams};
use smriti::db::memory as db_memory;
use smriti::types::*;

#[tokio::test]
async fn consolidation_creates_derives_edges() {
    let (db, _dir) = test_db().await;

    let a = create_test_memory(&db, "observation 1").await;
    let b = create_test_memory(&db, "observation 2").await;

    use smriti::tools::consolidate;
    let out = consolidate::execute(
        &db,
        None,
        consolidate::ConsolidateInput {
            ids: vec![a.as_str().to_string(), b.as_str().to_string()],
            result_summary: "combined observation".to_string(),
        },
    )
    .await
    .unwrap();

    // Verify derives edges exist from consolidated -> originals
    let mut result = db
        .query(
            "SELECT record::id(out) AS target FROM derives WHERE in = type::record('memory', $id)",
        )
        .bind(("id", out.id.clone()))
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = result.take(0).unwrap();

    let target_ids: Vec<String> = rows
        .iter()
        .filter_map(|r| r.get("target").and_then(|v| v.as_str()).map(String::from))
        .collect();

    assert!(
        target_ids.contains(&a.as_str().to_string()),
        "derives edge should point to original A"
    );
    assert!(
        target_ids.contains(&b.as_str().to_string()),
        "derives edge should point to original B"
    );

    // Originals should be archived
    let mem_a = db_memory::get(&db, &a).await.unwrap();
    let mem_b = db_memory::get(&db, &b).await.unwrap();
    assert_eq!(mem_a.status, MemoryStatus::Archived);
    assert_eq!(mem_b.status, MemoryStatus::Archived);
}

/// `consolidate_atomic` must compute aggregated metadata from ALL source records:
/// - importance = max(sources)
/// - people     = union(sources), deduped
/// - tags       = union(sources), deduped
#[tokio::test]
async fn consolidate_aggregates_max_importance_and_unions_people_tags() {
    let (db, _dir) = test_db().await;

    use smriti::tools::consolidate;

    // Source 1: importance=2, people=["alice"], tags=["x"]
    let m1 = db_memory::create(
        &db,
        db_memory::MemoryDraft::new(
            "first source fact about aggregation importance test".to_string(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli),
            Some(Modality::Text),
        )
        .project("agg-test".into())
        .importance(Importance::new(2).unwrap())
        .people(vec!["alice".to_string()])
        .tags(vec!["x".to_string()]),
    )
    .await
    .unwrap();

    // Source 2: importance=4, people=["bob", "carol"], tags=["y"]
    let m2 = db_memory::create(
        &db,
        db_memory::MemoryDraft::new(
            "second source fact about aggregation importance test".to_string(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli),
            Some(Modality::Text),
        )
        .project("agg-test".into())
        .importance(Importance::new(4).unwrap())
        .people(vec!["bob".to_string(), "carol".to_string()])
        .tags(vec!["y".to_string()]),
    )
    .await
    .unwrap();

    // Source 3: importance=3, people=["alice"], tags=["x", "z"]
    let m3 = db_memory::create(
        &db,
        db_memory::MemoryDraft::new(
            "third source fact about aggregation importance test".to_string(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli),
            Some(Modality::Text),
        )
        .project("agg-test".into())
        .importance(Importance::new(3).unwrap())
        .people(vec!["alice".to_string()])
        .tags(vec!["x".to_string(), "z".to_string()]),
    )
    .await
    .unwrap();

    let out = consolidate::execute(
        &db,
        None,
        consolidate::ConsolidateInput {
            ids: vec![
                m1.as_str().to_string(),
                m2.as_str().to_string(),
                m3.as_str().to_string(),
            ],
            result_summary: "consolidated aggregation test memory combining all three sources"
                .to_string(),
        },
    )
    .await
    .unwrap();

    // Fetch the merged memory via raw query to inspect all fields.
    let merged_id = out.id.clone();
    let mut result = db
        .query("SELECT importance, people, tags FROM type::record('memory', $id)")
        .bind(("id", merged_id.clone()))
        .await
        .unwrap();
    let row: Option<serde_json::Value> = result.take(0).unwrap();
    let row = row.expect("merged memory must exist");

    // importance = max(2, 4, 3) = 4
    let importance = row.get("importance").and_then(|v| v.as_u64()).unwrap_or(0);
    assert_eq!(
        importance, 4,
        "merged importance must be max(sources) = 4; got {importance}"
    );

    // people = union(["alice"], ["bob","carol"], ["alice"]) = {alice, bob, carol}
    let people: Vec<String> = row
        .get("people")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let mut people_sorted = people.clone();
    people_sorted.sort();
    assert_eq!(
        people_sorted,
        vec!["alice", "bob", "carol"],
        "merged people must be union of all sources: {{alice, bob, carol}}; got {people:?}"
    );

    // tags = union(["x"], ["y"], ["x","z"]) = {x, y, z}
    let tags: Vec<String> = row
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let mut tags_sorted = tags.clone();
    tags_sorted.sort();
    assert_eq!(
        tags_sorted,
        vec!["x", "y", "z"],
        "merged tags must be union of all sources: {{x, y, z}}; got {tags:?}"
    );
}

/// A cluster containing a Private memory must be filtered before reaching the sidecar.
///
/// `fetch_memories_by_ids` with `BITEMPORAL_ACTIVE` returns the Private memory;
/// the visibility check in the auto-consolidation loop (`!= Standard`) catches it
/// and must reject the cluster without calling synthesize_consolidation.
///
/// This test verifies the filter predicate on the returned `MemoryMinimal` structs:
/// at least one member has `visibility != Standard`, so the loop must skip it.
#[tokio::test]
async fn fetch_memories_by_ids_private_visibility_detectable() {
    let (db, _dir) = test_db().await;

    // Standard memory
    let std_id = db_memory::create(
        &db,
        db_memory::MemoryDraft::new(
            "standard visibility memory for cluster filter test".to_string(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli),
            Some(Modality::Text),
        )
        .visibility(Visibility::Standard),
    )
    .await
    .unwrap();

    // Private memory — same cluster
    let priv_id = db_memory::create(
        &db,
        db_memory::MemoryDraft::new(
            "private visibility memory for cluster filter test".to_string(),
            MemoryType::Semantic,
            MemorySubtype::Fact,
            Some(Channel::Cli),
            Some(Modality::Text),
        )
        .visibility(Visibility::Private),
    )
    .await
    .unwrap();

    let ids = vec![std_id.clone(), priv_id.clone()];
    let sources = fetch_memories_by_ids(&db, &ids).await.unwrap();

    // Both active memories must be returned (BITEMPORAL_ACTIVE does not filter by visibility).
    assert_eq!(
        sources.len(),
        2,
        "fetch_memories_by_ids must return both active memories regardless of visibility; got {}",
        sources.len()
    );

    // The auto-consolidation loop guard: any member != Standard → reject cluster.
    let has_non_standard = sources.iter().any(|m| m.visibility != Visibility::Standard);
    assert!(
        has_non_standard,
        "cluster containing a Private memory must be detectable via visibility != Standard check"
    );

    // Confirm the private one is indeed Private.
    let private_member = sources.iter().find(|m| m.id == priv_id);
    assert!(
        private_member.is_some(),
        "Private memory must appear in fetch_memories_by_ids result"
    );
    assert_eq!(
        private_member.unwrap().visibility,
        Visibility::Private,
        "Private memory visibility must be preserved through fetch"
    );
}

/// Heuristic-1 of `find_candidates` must surface a cluster when 3+ active Standard
/// memories share the same project, subtype, and author — regression guard for the
/// `array::group` SurrealKV silent-empty bug (docs/surrealdb-v3.md §42-45).
///
/// The old GROUP BY / array::group approach returned 0 candidates on SurrealKV.
/// The Rust-side aggregation must return at least one candidate.
#[tokio::test]
async fn find_candidates_heuristic1_returns_cluster_for_same_project_subtype() {
    let (db, _dir) = test_db().await;

    // Create 3 Standard Semantic/Fact memories in the same project (no author = user-written).
    for i in 0..3_u8 {
        db_memory::create(
            &db,
            db_memory::MemoryDraft::new(
                format!("heuristic one cluster test memory number {i} for project regression"),
                MemoryType::Semantic,
                MemorySubtype::Fact,
                Some(Channel::Cli),
                Some(Modality::Text),
            )
            .project("h1-regression-project".into())
            .visibility(Visibility::Standard),
        )
        .await
        .unwrap();
    }

    let candidates = smriti::db::consolidation::find_candidates(
        &db,
        10,
        None,  // no age filter
        None,  // no cluster size cap
    )
    .await
    .unwrap();

    // At least one candidate from heuristic-1 with our project.
    let h1_candidate = candidates.iter().find(|c| {
        c.project.as_deref() == Some("h1-regression-project")
            && c.reason.contains("Same project + subtype")
    });

    assert!(
        h1_candidate.is_some(),
        "heuristic-1 must return a candidate for 3 memories with the same project+subtype; \
         got {} total candidates: {candidates:?}",
        candidates.len()
    );

    let candidate = h1_candidate.unwrap();
    assert!(
        candidate.ids.len() >= 3,
        "candidate cluster must contain at least 3 members; got {}",
        candidate.ids.len()
    );
}

/// `consolidate_atomic` must not leave an orphan consolidated memory when it
/// fails — verifying the error-propagation fix where `response.take(idx)`
/// errors were previously swallowed by `unwrap_or_default()`.
///
/// Strategy: archive all source memories before calling `consolidate_atomic`.
/// `fetch_source_metadata` returns `InvalidInput` (no active sources), so the
/// transaction never runs.  The new `MemoryId` (generated inside
/// `consolidate_atomic`) must not exist in the DB afterwards.
///
/// A second sub-case exercises `consolidate_atomic` on completely fabricated
/// (never-inserted) IDs to confirm the same invariant holds regardless of
/// whether the IDs were ever valid.
#[tokio::test]
async fn consolidate_atomic_failure_leaves_no_orphan_memory() {
    let (db, _dir) = test_db().await;

    // ── Sub-case 1: sources exist then are archived before consolidation ──
    let src_a = create_test_memory(&db, "source memory alpha for orphan test").await;
    let src_b = create_test_memory(&db, "source memory beta for orphan test").await;

    // Archive both sources, simulating a race between candidate selection and consolidation.
    db_memory::archive(&db, &src_a, smriti::types::InvalidationReason::Forgotten)
        .await
        .expect("archive src_a");
    db_memory::archive(&db, &src_b, smriti::types::InvalidationReason::Forgotten)
        .await
        .expect("archive src_b");

    let result = consolidate_atomic(
        &db,
        &[src_a.clone(), src_b.clone()],
        ConsolidateParams {
            summary: "should not be created because all sources are archived".to_string(),
            vector: None,
            source_agent: None,
            event_at: None,
        },
    )
    .await;

    assert!(
        result.is_err(),
        "consolidate_atomic must return Err when all sources are archived; got Ok"
    );

    // The new memory must NOT have been written — no orphan row.
    // We can't get the generated MemoryId from the Err variant directly, so we
    // count total active memories: should still be 0 (both sources are archived).
    let mut count_result = db
        .query("SELECT count() AS n FROM memory WHERE status = 'active' GROUP ALL")
        .await
        .unwrap();
    let count_row: Option<serde_json::Value> = count_result.take(0).unwrap();
    let active_count = count_row
        .and_then(|r| r.get("n").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    assert_eq!(
        active_count, 0,
        "no active memories should exist after failed consolidation; found {active_count}"
    );

    // ── Sub-case 2: completely fabricated IDs (never inserted) ──
    let fake_id_a = MemoryId::from("00000000-0000-0000-0000-000000000001".to_string());
    let fake_id_b = MemoryId::from("00000000-0000-0000-0000-000000000002".to_string());

    let result2 = consolidate_atomic(
        &db,
        &[fake_id_a, fake_id_b],
        ConsolidateParams {
            summary: "should not be created because source IDs are fabricated".to_string(),
            vector: None,
            source_agent: None,
            event_at: None,
        },
    )
    .await;

    assert!(
        result2.is_err(),
        "consolidate_atomic must return Err for fabricated source IDs; got Ok"
    );

    // Still 0 active memories.
    let mut count_result2 = db
        .query("SELECT count() AS n FROM memory WHERE status = 'active' GROUP ALL")
        .await
        .unwrap();
    let count_row2: Option<serde_json::Value> = count_result2.take(0).unwrap();
    let active_count2 = count_row2
        .and_then(|r| r.get("n").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    assert_eq!(
        active_count2, 0,
        "no active memories should exist after second failed consolidation; found {active_count2}"
    );
}
