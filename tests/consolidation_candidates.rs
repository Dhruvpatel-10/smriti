//! Tests for `find_candidates` — the two-heuristic consolidation candidate detector.
//!
//! Heuristic 1: same project + same subtype, ≥3 active standard memories.
//! Heuristic 2: graph cluster — a memory with ≥2 outbound `relates` edges to standard memories.
//!
//! Gap: `consolidation_candidates_hides_sensitive` is already covered in
//! `tests/visibility.rs::consolidation_candidates_hide_sensitive`.

mod common;
use common::test_db;

use smriti::db::{consolidation, graph, memory};
use smriti::tools::agent as agent_tools;
use smriti::types::*;

// ═══════════════════════════════════════════════════════════════════════════
// Heuristic 0 — empty database
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn consolidation_candidates_empty_db_returns_empty() {
    let (db, _dir) = test_db().await;

    let candidates = consolidation::find_candidates(&db, 10, None, None).await.unwrap();

    assert!(
        candidates.is_empty(),
        "empty DB should return no consolidation candidates"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Heuristic 1 — project + subtype cluster with ≥3 active standard members
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn consolidation_candidates_detects_project_subtype_cluster() {
    let (db, _dir) = test_db().await;

    // Create 4 standard Semantic/Fact memories in the same project.
    let mut ids = Vec::new();
    for i in 0..4usize {
        let id = memory::create(&db,
        memory::MemoryDraft::new(format!("Cluster fact {i} about the shared topic"), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .project("cluster-proj".into())
            .visibility(Visibility::Standard))
        .await
        .unwrap();
        ids.push(id.as_str().to_string());
    }

    let candidates = consolidation::find_candidates(&db, 10, None, None).await.unwrap();

    assert!(
        !candidates.is_empty(),
        "project+subtype cluster of 4 should produce at least one candidate"
    );

    // Find the cluster that matches our project.
    let cluster = candidates
        .iter()
        .find(|c| c.project.as_deref() == Some("cluster-proj"))
        .expect("expected a candidate for 'cluster-proj'");

    // All 4 ids must appear in the cluster.
    for id in &ids {
        assert!(
            cluster.ids.contains(id),
            "expected id {id} in cluster ids: {:?}",
            cluster.ids
        );
    }
}

#[tokio::test]
async fn consolidation_candidates_cluster_reason_mentions_subtype() {
    let (db, _dir) = test_db().await;

    for i in 0..3usize {
        memory::create(&db,
        memory::MemoryDraft::new(format!("Pattern observation {i} for the system design"), MemoryType::Semantic, MemorySubtype::Pattern, Some(Channel::Cli), Some(Modality::Text))
            .project("reason-proj".into())
            .visibility(Visibility::Standard))
        .await
        .unwrap();
    }

    let candidates = consolidation::find_candidates(&db, 10, None, None).await.unwrap();

    let cluster = candidates
        .iter()
        .find(|c| c.project.as_deref() == Some("reason-proj"))
        .expect("expected a candidate for 'reason-proj'");

    assert!(
        cluster.reason.contains("pattern"),
        "reason should mention the subtype 'pattern', got: {}",
        cluster.reason
    );
}

#[tokio::test]
async fn consolidation_candidates_below_threshold_not_returned() {
    let (db, _dir) = test_db().await;

    // Only 2 memories — below the ≥3 threshold.
    for i in 0..2usize {
        memory::create(&db,
        memory::MemoryDraft::new(format!("Sparse fact {i} in a small project"), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .project("sparse-proj".into())
            .visibility(Visibility::Standard))
        .await
        .unwrap();
    }

    let candidates = consolidation::find_candidates(&db, 10, None, None).await.unwrap();

    assert!(
        !candidates
            .iter()
            .any(|c| c.project.as_deref() == Some("sparse-proj")),
        "a cluster with only 2 members should NOT be returned (threshold is 3)"
    );
}

#[tokio::test]
async fn consolidation_candidates_private_memories_excluded() {
    let (db, _dir) = test_db().await;

    // 3 private memories in same project+subtype — should not form a cluster
    // (find_candidates filters WHERE visibility = 'standard').
    for i in 0..3usize {
        memory::create(&db,
        memory::MemoryDraft::new(format!("Private fact {i} that should not cluster"), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .project("private-proj".into())
            .visibility(Visibility::Private))
        .await
        .unwrap();
    }

    let candidates = consolidation::find_candidates(&db, 10, None, None).await.unwrap();

    assert!(
        !candidates
            .iter()
            .any(|c| c.project.as_deref() == Some("private-proj")),
        "private memories should be excluded from consolidation candidates"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Heuristic 2 — graph cluster via ≥2 relates edges
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn consolidation_candidates_detects_graph_cluster() {
    let (db, _dir) = test_db().await;

    // Hub memory that will have 2 outbound `relates` edges.
    let hub = memory::create(&db,
        memory::MemoryDraft::new("Hub memory: central node with multiple relates edges".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .visibility(Visibility::Standard))
    .await
    .unwrap();

    let spoke_a = memory::create(&db,
        memory::MemoryDraft::new("Spoke A: related to hub via relates edge alpha".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .visibility(Visibility::Standard))
    .await
    .unwrap();

    let spoke_b = memory::create(&db,
        memory::MemoryDraft::new("Spoke B: related to hub via relates edge beta".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .visibility(Visibility::Standard))
    .await
    .unwrap();

    // Link hub -> spoke_a and hub -> spoke_b via `relates`.
    graph::link_memories(&db, &hub, &spoke_a, RelationType::Relates)
        .await
        .unwrap();
    graph::link_memories(&db, &hub, &spoke_b, RelationType::Relates)
        .await
        .unwrap();

    let candidates = consolidation::find_candidates(&db, 10, None, None).await.unwrap();

    // At least one graph cluster must be returned.
    let graph_clusters: Vec<_> = candidates
        .iter()
        .filter(|c| c.reason.contains("Graph cluster"))
        .collect();

    assert!(
        !graph_clusters.is_empty(),
        "hub with 2 relates edges should appear as a graph cluster candidate"
    );

    // The hub id must be in one of the graph cluster entries.
    let hub_str = hub.as_str().to_string();
    let hub_in_cluster = graph_clusters.iter().any(|c| c.ids.contains(&hub_str));
    assert!(
        hub_in_cluster,
        "hub memory id should appear in a graph cluster candidate"
    );
}

#[tokio::test]
async fn consolidation_candidates_single_relates_edge_not_a_cluster() {
    let (db, _dir) = test_db().await;

    // Hub with only 1 relates edge — below the ≥2 threshold.
    let hub = memory::create(&db,
        memory::MemoryDraft::new("Hub with single edge: should not form a graph cluster".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .visibility(Visibility::Standard))
    .await
    .unwrap();

    let spoke = memory::create(&db,
        memory::MemoryDraft::new("Spoke: only one relates link to hub memory node".into(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .visibility(Visibility::Standard))
    .await
    .unwrap();

    graph::link_memories(&db, &hub, &spoke, RelationType::Relates)
        .await
        .unwrap();

    let candidates = consolidation::find_candidates(&db, 10, None, None).await.unwrap();

    let hub_str = hub.as_str().to_string();
    let hub_is_graph_cluster_hub = candidates.iter().any(|c| {
        c.reason.contains("Graph cluster") && c.ids.first().is_some_and(|id| *id == hub_str)
    });

    assert!(
        !hub_is_graph_cluster_hub,
        "a memory with only 1 relates edge should NOT be a graph cluster hub"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Limit enforcement
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn consolidation_candidates_respects_limit() {
    let (db, _dir) = test_db().await;

    // Create two separate project+subtype clusters, each with 3 members.
    for proj in ["limit-proj-a", "limit-proj-b"] {
        for i in 0..3usize {
            memory::create(&db,
        memory::MemoryDraft::new(format!("Limit test fact {i} in project {proj}"), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .project(proj.into())
            .visibility(Visibility::Standard))
            .await
            .unwrap();
        }
    }

    // With limit=1 only one candidate should be returned.
    let candidates = consolidation::find_candidates(&db, 1, None, None).await.unwrap();

    assert_eq!(
        candidates.len(),
        1,
        "find_candidates(limit=1) must return at most 1 candidate"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Author isolation — shared daemon multi-tenancy
// ═══════════════════════════════════════════════════════════════════════════

/// Two agents writing into the same project+subtype must never share a cluster.
///
/// On a shared daemon two users' memories may have the same `project` string
/// and the same `subtype`. Without `GROUP BY … author` the old query would
/// merge them into a single cross-author cluster. This test asserts that
/// heuristic 1 produces exactly two separate clusters — one per author — and
/// that no cluster mixes IDs from both agents.
#[tokio::test]
async fn find_candidates_does_not_cross_author_boundary() {
    let (db, _dir) = test_db().await;

    // Register two distinct agents.
    let out_a = agent_tools::execute_register(
        &db,
        agent_tools::AgentRegisterInput {
            name: "cross-author-agent-a".to_string(),
            kind: AgentKind::Assistant,
            model: None,
            version: None,
            parent: None,
            project: None,
            config: None,
        },
    )
    .await
    .expect("register agent_a failed");

    let out_b = agent_tools::execute_register(
        &db,
        agent_tools::AgentRegisterInput {
            name: "cross-author-agent-b".to_string(),
            kind: AgentKind::Reviewer,
            model: None,
            version: None,
            parent: None,
            project: None,
            config: None,
        },
    )
    .await
    .expect("register agent_b failed");

    let agent_a_id = out_a.id;
    let agent_b_id = out_b.id;

    // Write 3 Semantic/Fact memories under "shared" project authored by agent_a.
    let mut ids_a = Vec::new();
    for i in 0..3usize {
        let id = memory::create(
            &db,
            memory::MemoryDraft::new(
                format!("Cross-author fact {i} written by agent A about the system"),
                MemoryType::Semantic,
                MemorySubtype::Fact,
                Some(Channel::Cli),
                Some(Modality::Text),
            )
            .project("shared".into())
            .visibility(Visibility::Standard)
            .author(agent_a_id.clone()),
        )
        .await
        .expect("create memory for agent_a failed");
        ids_a.push(id.as_str().to_string());
    }

    // Write 3 Semantic/Fact memories under the same project authored by agent_b.
    let mut ids_b = Vec::new();
    for i in 0..3usize {
        let id = memory::create(
            &db,
            memory::MemoryDraft::new(
                format!("Cross-author fact {i} written by agent B about the system"),
                MemoryType::Semantic,
                MemorySubtype::Fact,
                Some(Channel::Cli),
                Some(Modality::Text),
            )
            .project("shared".into())
            .visibility(Visibility::Standard)
            .author(agent_b_id.clone()),
        )
        .await
        .expect("create memory for agent_b failed");
        ids_b.push(id.as_str().to_string());
    }

    let candidates = consolidation::find_candidates(&db, 10, None, None).await.unwrap();

    // Filter to only the clusters for the "shared" project.
    let shared_clusters: Vec<_> = candidates
        .iter()
        .filter(|c| c.project.as_deref() == Some("shared"))
        .collect();

    assert_eq!(
        shared_clusters.len(),
        2,
        "expected exactly 2 clusters (one per author), got {}: {:?}",
        shared_clusters.len(),
        shared_clusters.iter().map(|c| &c.ids).collect::<Vec<_>>()
    );

    // Each cluster must contain exactly 3 ids.
    for cluster in &shared_clusters {
        assert_eq!(
            cluster.ids.len(),
            3,
            "each per-author cluster should have exactly 3 ids, got {:?}",
            cluster.ids
        );
    }

    // Verify no cluster contains IDs from both agents.
    for cluster in &shared_clusters {
        let has_a = cluster.ids.iter().any(|id| ids_a.contains(id));
        let has_b = cluster.ids.iter().any(|id| ids_b.contains(id));
        assert!(
            !(has_a && has_b),
            "cluster must not mix agent_a and agent_b IDs: {:?}",
            cluster.ids
        );
    }

    // Verify that one cluster belongs to agent_a and one to agent_b.
    let cluster_authors: std::collections::HashSet<Option<&AgentId>> =
        shared_clusters.iter().map(|c| c.author.as_ref()).collect();
    assert!(
        cluster_authors.contains(&Some(&agent_a_id)),
        "expected a cluster authored by agent_a ({agent_a_id}), got authors: {cluster_authors:?}"
    );
    assert!(
        cluster_authors.contains(&Some(&agent_b_id)),
        "expected a cluster authored by agent_b ({agent_b_id}), got authors: {cluster_authors:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// min_age_days filter (Change 3)
// ═══════════════════════════════════════════════════════════════════════════

/// Memories created "now" must be excluded when `min_age_days=Some(7)`.
/// Without the filter (`None`), the same cluster must appear.
#[tokio::test]
async fn find_candidates_min_age_filter_excludes_recent() {
    let (db, _dir) = test_db().await;

    // Write 3 memories in the same project+subtype (all created at ~now).
    for i in 0..3usize {
        memory::create(
            &db,
            memory::MemoryDraft::new(
                format!("Recent memory {i} about min-age filter test case scenario"),
                MemoryType::Semantic,
                MemorySubtype::Fact,
                Some(Channel::Cli),
                Some(Modality::Text),
            )
            .project("min-age-proj".into())
            .visibility(Visibility::Standard),
        )
        .await
        .unwrap();
    }

    // With min_age_days=7: all memories are younger than 7 days → no candidates.
    let candidates_with_age = consolidation::find_candidates(&db, 10, Some(7), None)
        .await
        .unwrap();
    assert!(
        !candidates_with_age
            .iter()
            .any(|c| c.project.as_deref() == Some("min-age-proj")),
        "recent memories (< 7d) must be excluded by min_age_days=7 filter"
    );

    // Without age filter (None): same cluster must appear.
    let candidates_no_age = consolidation::find_candidates(&db, 10, None, None)
        .await
        .unwrap();
    assert!(
        candidates_no_age
            .iter()
            .any(|c| c.project.as_deref() == Some("min-age-proj")),
        "without age filter the cluster of 3 must be returned"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// max_cluster_size filter (Change 3)
// ═══════════════════════════════════════════════════════════════════════════

/// A cluster with 5 members must be excluded when `max_cluster_size=Some(4)`.
/// Without the cap (`None`), the same cluster must appear.
#[tokio::test]
async fn find_candidates_max_cluster_size_excludes_oversized_cluster() {
    let (db, _dir) = test_db().await;

    // Write 5 memories in the same project+subtype.
    for i in 0..5usize {
        memory::create(
            &db,
            memory::MemoryDraft::new(
                format!("Oversized cluster memory {i} about max-size filter test scenario"),
                MemoryType::Semantic,
                MemorySubtype::Fact,
                Some(Channel::Cli),
                Some(Modality::Text),
            )
            .project("max-size-proj".into())
            .visibility(Visibility::Standard),
        )
        .await
        .unwrap();
    }

    // max_cluster_size=4: cluster of 5 exceeds cap → no candidates for this project.
    let candidates_capped = consolidation::find_candidates(&db, 10, None, Some(4))
        .await
        .unwrap();
    assert!(
        !candidates_capped
            .iter()
            .any(|c| c.project.as_deref() == Some("max-size-proj")),
        "cluster of 5 must be excluded when max_cluster_size=4"
    );

    // Without cap (None): cluster of 5 must appear (≥3 threshold satisfied).
    let candidates_uncapped = consolidation::find_candidates(&db, 10, None, None)
        .await
        .unwrap();
    assert!(
        candidates_uncapped
            .iter()
            .any(|c| c.project.as_deref() == Some("max-size-proj")),
        "without size cap the cluster of 5 must be returned"
    );
}
