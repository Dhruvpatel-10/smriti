mod common;
use common::test_db;

use smriti::db::{auto_link, embeddings, memory};
use smriti::types::*;

// ═══════════════════════════════════════════════════════════════════════════
// HELPERS
// ═══════════════════════════════════════════════════════════════════════════

/// Produce two unit vectors of `dim` dimensions whose cosine similarity equals
/// `target`.  The first vector is the standard basis e₀ = [1, 0, …].  The
/// second vector is [target, sqrt(1 - target²), 0, …], which is a unit vector
/// with dot-product `target` against the first.
///
/// Requires dim ≥ 2 and 0 ≤ target ≤ 1.
fn unit_vec_pair_with_cosine(target: f32, dim: usize) -> (Vec<f32>, Vec<f32>) {
    assert!(dim >= 2, "need at least 2 dimensions");
    let mut a = vec![0.0f32; dim];
    let mut b = vec![0.0f32; dim];
    a[0] = 1.0;
    b[0] = target;
    b[1] = (1.0_f32 - target * target).sqrt();
    (a, b)
}

async fn create_memory_with_project(
    db: &smriti::db::client::SurrealClient,
    summary: &str,
    project: Option<&str>,
) -> MemoryId {
    memory::create(db,
        memory::MemoryDraft::new(summary.to_string(), MemoryType::Semantic, MemorySubtype::Fact, Some(Channel::Cli), Some(Modality::Text))
            .maybe_project(project.map(String::from)))
    .await
    .expect("test memory creation failed")
}

/// Returns the number of edges in `table` between `from` and `to` (in either
/// direction — auto_link_similar always links new→existing).
async fn edge_count(
    db: &smriti::db::client::SurrealClient,
    table: &str,
    id_a: &MemoryId,
    id_b: &MemoryId,
) -> usize {
    let sql = format!(
        "SELECT * FROM {table} WHERE (in = type::record('memory', $a) AND out = type::record('memory', $b))
                                  OR (in = type::record('memory', $b) AND out = type::record('memory', $a))"
    );
    let mut resp = db
        .query(&sql)
        .bind(("a", id_a.as_str().to_string()))
        .bind(("b", id_b.as_str().to_string()))
        .await
        .expect("edge query failed");
    let rows: Vec<serde_json::Value> = resp.take(0).unwrap_or_default();
    rows.len()
}

// ═══════════════════════════════════════════════════════════════════════════
// THRESHOLD TESTS
// ═══════════════════════════════════════════════════════════════════════════

/// cosine ≈ 0.95, same project → `extends` edge
#[tokio::test]
async fn auto_link_same_project_extends_above_0_90() {
    let (db, _dir) = test_db().await;
    const DIM: usize = 256;
    let (vec_a, vec_b) = unit_vec_pair_with_cosine(0.95, DIM);

    let id_a = create_memory_with_project(&db, "decision about proj alpha", Some("proj-a")).await;
    let id_b = create_memory_with_project(&db, "follow up on proj alpha decision", Some("proj-a")).await;

    embeddings::store_vector_pub(&db, id_a.as_str(), vec_a).await.unwrap();
    embeddings::store_vector_pub(&db, id_b.as_str(), vec_b.clone()).await.unwrap();

    let links = auto_link::auto_link_similar(&db, &id_b, Some("proj-a"), vec_b)
        .await
        .unwrap();

    assert!(links >= 1, "expected at least one link, got {links}");
    let count = edge_count(&db, "extends", &id_a, &id_b).await;
    assert_eq!(count, 1, "expected an `extends` edge between the two memories");
}

/// cosine ≈ 0.80, same project → `relates` edge (not `extends`)
#[tokio::test]
async fn auto_link_same_project_relates_between_0_75_and_0_90() {
    let (db, _dir) = test_db().await;
    const DIM: usize = 256;
    let (vec_a, vec_b) = unit_vec_pair_with_cosine(0.80, DIM);

    let id_a = create_memory_with_project(&db, "backend architecture notes", Some("proj-b")).await;
    let id_b = create_memory_with_project(&db, "backend service design rationale", Some("proj-b")).await;

    embeddings::store_vector_pub(&db, id_a.as_str(), vec_a).await.unwrap();
    embeddings::store_vector_pub(&db, id_b.as_str(), vec_b.clone()).await.unwrap();

    let links = auto_link::auto_link_similar(&db, &id_b, Some("proj-b"), vec_b)
        .await
        .unwrap();

    assert!(links >= 1, "expected at least one relates link, got {links}");

    let relates_count = edge_count(&db, "relates", &id_a, &id_b).await;
    assert_eq!(relates_count, 1, "expected a `relates` edge");

    let extends_count = edge_count(&db, "extends", &id_a, &id_b).await;
    assert_eq!(extends_count, 0, "must not create an `extends` edge at 0.80");
}

/// cosine ≈ 0.80, different projects → NO edge (below cross-domain threshold 0.85)
#[tokio::test]
async fn auto_link_cross_project_no_edge_below_0_85() {
    let (db, _dir) = test_db().await;
    const DIM: usize = 256;
    let (vec_a, vec_b) = unit_vec_pair_with_cosine(0.80, DIM);

    let id_a = create_memory_with_project(&db, "frontend theme preference", Some("proj-x")).await;
    let id_b = create_memory_with_project(&db, "color scheme design note", Some("proj-y")).await;

    embeddings::store_vector_pub(&db, id_a.as_str(), vec_a).await.unwrap();
    embeddings::store_vector_pub(&db, id_b.as_str(), vec_b.clone()).await.unwrap();

    let links = auto_link::auto_link_similar(&db, &id_b, Some("proj-y"), vec_b)
        .await
        .unwrap();

    assert_eq!(links, 0, "cross-project at 0.80 must create no links");

    let relates_count = edge_count(&db, "relates", &id_a, &id_b).await;
    assert_eq!(relates_count, 0, "no `relates` edge expected");
}

/// cosine ≈ 0.87, different projects → `relates` edge (above cross-domain threshold 0.85)
#[tokio::test]
async fn auto_link_cross_project_relates_above_0_85() {
    let (db, _dir) = test_db().await;
    const DIM: usize = 256;
    let (vec_a, vec_b) = unit_vec_pair_with_cosine(0.87, DIM);

    let id_a = create_memory_with_project(&db, "api versioning strategy overview", Some("proj-m")).await;
    let id_b = create_memory_with_project(&db, "versioning policy cross team", Some("proj-n")).await;

    embeddings::store_vector_pub(&db, id_a.as_str(), vec_a).await.unwrap();
    embeddings::store_vector_pub(&db, id_b.as_str(), vec_b.clone()).await.unwrap();

    let links = auto_link::auto_link_similar(&db, &id_b, Some("proj-n"), vec_b)
        .await
        .unwrap();

    assert!(links >= 1, "expected at least one cross-project relates link, got {links}");

    let relates_count = edge_count(&db, "relates", &id_a, &id_b).await;
    assert_eq!(relates_count, 1, "expected a cross-domain `relates` edge");
}
