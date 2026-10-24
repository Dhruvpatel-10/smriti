use std::collections::{HashMap, HashSet};
use std::time::Instant;

use chrono::{DateTime, Duration, Utc};
use surrealdb::types::RecordId;
use tracing::{instrument, warn};

use crate::config::{FusionConfig, FusionStrategy, MmrConfig, SpreadingConfig};
use crate::db::client::SurrealClient;
use crate::db::entity_alias;
use crate::db::helpers::enum_to_value;
use crate::db::memory::parse_memory_row;
use crate::db::recall_metrics::{self, RecallStage};
use crate::db::reranker::{RerankCandidate, Reranker};
use crate::db::BITEMPORAL_ACTIVE;
use crate::error::{KbError, Result};
use crate::types::*;

// ── Author filter ───────────────────────────────────────────────────────────

/// Resolved author predicate for recall queries.
///
/// `None` → no author predicate (returns all memories regardless of author).
/// `UserOnly` → `WHERE author IS NONE` (only user-authored memories).
/// `Agent(id)` → `WHERE author = <agent_record>` (only that agent's memories).
#[derive(Debug, Clone)]
pub enum AuthorFilter {
    /// No author predicate — all memories visible to the caller.
    None,
    /// `"me"` — user-authored memories only (author IS NONE).
    UserOnly,
    /// Named agent — memories authored by this specific agent.
    Agent(AgentId),
}

// ── Reciprocal Rank Fusion ──────────────────────────────────────────────────

/// RRF constant k — controls how much rank matters vs. flat contribution.
/// k=60 is the standard from Cormack et al. (2009).
const RRF_K: f64 = 60.0;

/// SM-2 interval ceiling in days. Caps the scheduled review delay at one year — both
/// a product decision (no memory should drift beyond annual review) and protection
/// against f64→i64 overflow on long-lived items before the cast.
const MAX_INTERVAL_DAYS: f64 = 365.0;

fn rrf_score(rank: usize) -> f64 {
    1.0 / (RRF_K + rank as f64 + 1.0)
}

/// Fuse two ranked result lists into a single list via Reciprocal Rank Fusion.
///
/// Each source contributes `1/(k + rank + 1)` per item. Items appearing in both
/// sources get summed scores. Returns `(memories, rrf_scores)` where `rrf_scores`
/// is the parallel list of fused scores in descending order, truncated to `limit`.
/// The scores are needed by `should_rerank` (ADR-022) for dominance detection.
fn rrf_fuse(
    bm25: &[MemorySummary],
    vector: &[MemorySummary],
    limit: usize,
    min_score: f64,
) -> (Vec<MemorySummary>, Vec<f64>) {
    // Single map: id string → (accumulated score, first-seen MemorySummary).
    // Avoids the second HashMap and the redundant clone from the two-map approach.
    let mut fused: HashMap<String, (f64, MemorySummary)> = HashMap::new();

    for (rank, mem) in bm25.iter().enumerate() {
        let id = mem.id.as_str().to_string();
        let entry = fused.entry(id).or_insert_with(|| (0.0, mem.clone()));
        entry.0 += rrf_score(rank);
    }
    for (rank, mem) in vector.iter().enumerate() {
        let id = mem.id.as_str().to_string();
        let entry = fused.entry(id).or_insert_with(|| (0.0, mem.clone()));
        entry.0 += rrf_score(rank);
    }

    let mut ranked: Vec<(f64, MemorySummary)> = fused
        .into_values()
        .filter(|(score, _)| *score >= min_score)
        .collect();

    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(limit);
    let scores: Vec<f64> = ranked.iter().map(|(s, _)| *s).collect();
    let memories: Vec<MemorySummary> = ranked.into_iter().map(|(_, m)| m).collect();
    (memories, scores)
}

// ── Score extraction helpers ────────────────────────────────────────────────

/// Extract `_bm25_score` from a raw DB row, falling back to 0.0.
fn row_bm25_score(row: &serde_json::Value) -> f64 {
    row.get("_bm25_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

/// Extract `_vscore` (cosine similarity) from a raw DB row, falling back to 0.0.
fn row_vscore(row: &serde_json::Value) -> f64 {
    row.get("_vscore")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

/// Build parallel score arrays from raw rows: `(bm25_scores, vscore_or_none)`.
fn extract_scores(rows: &[serde_json::Value]) -> Vec<f64> {
    rows.iter().map(row_bm25_score).collect()
}

/// Build vector-score parallel array from raw rows.
fn extract_vscores(rows: &[serde_json::Value]) -> Vec<f64> {
    rows.iter().map(row_vscore).collect()
}

// ── Convex Combination fusion ────────────────────────────────────────────────

/// Min-max normalise a score list into [0, 1].
///
/// If all scores are equal (range == 0), returns all-1.0 — avoids div-by-zero
/// while treating a constant list as uniformly confident.
fn min_max_normalize(scores: impl Iterator<Item = f64>) -> Vec<f64> {
    let v: Vec<f64> = scores.collect();
    if v.is_empty() {
        return v;
    }
    let min = v.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let range = max - min;
    if range == 0.0 {
        return v.iter().map(|_| 1.0).collect();
    }
    v.iter().map(|s| (s - min) / range).collect()
}

/// Convex Combination fusion of BM25 and vector results.
///
/// Score = α · normalize(vector_score) + (1-α) · normalize(bm25_score).
/// Min-max normalisation per result list ensures both scores land in [0, 1].
/// Items appearing in only one list get a 0.0 contribution from the absent list.
fn convex_fuse(
    bm25: &[MemorySummary],
    bm25_scores: &[f64],
    vector: &[MemorySummary],
    vec_scores: &[f64],
    vector_weight: f64,
    limit: usize,
    min_score: f64,
) -> (Vec<MemorySummary>, Vec<f64>) {
    let bm25_norm = min_max_normalize(bm25_scores.iter().copied());
    let vec_norm = min_max_normalize(vec_scores.iter().copied());

    let bm25_w = 1.0 - vector_weight;

    // id → (accumulated CC score, MemorySummary)
    let mut scores: HashMap<String, (f64, MemorySummary)> = HashMap::new();

    for ((mem, _raw), norm) in bm25.iter().zip(bm25_scores.iter()).zip(bm25_norm.iter()) {
        let id = mem.id.as_str().to_string();
        scores.entry(id).or_insert_with(|| (0.0, mem.clone())).0 += bm25_w * norm;
    }
    for ((mem, _raw), norm) in vector.iter().zip(vec_scores.iter()).zip(vec_norm.iter()) {
        let id = mem.id.as_str().to_string();
        let entry = scores.entry(id).or_insert_with(|| (0.0, mem.clone()));
        entry.0 += vector_weight * norm;
    }

    let mut ranked: Vec<(f64, MemorySummary)> = scores.into_values().collect();
    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    ranked.retain(|(s, _)| *s >= min_score);
    ranked.truncate(limit);

    let fused_scores: Vec<f64> = ranked.iter().map(|(s, _)| *s).collect();
    let mems: Vec<MemorySummary> = ranked.into_iter().map(|(_, m)| m).collect();
    (mems, fused_scores)
}

// ── MMR diversification ─────────────────────────────────────────────────────

/// Cosine similarity between two equal-length unit (or unnormalized) vectors.
///
/// Returns 0.0 for empty or mismatched-length inputs — safe to call on any DB row.
fn cosine_sim(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| (*x as f64) * (*y as f64)).sum();
    let norm_a: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Fetch stored embedding vectors for the given memory IDs in a single round-trip.
///
/// IDs not found or missing a `vector` field are silently omitted — the caller
/// treats an empty map as "no vectors available" and falls back to fusion order.
async fn fetch_candidate_vectors(
    db: &SurrealClient,
    ids: &[MemoryId],
) -> Result<HashMap<MemoryId, Vec<f32>>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let id_strings: Vec<String> = ids.iter().map(|i| i.as_str().to_string()).collect();
    let mut res = db
        .query("SELECT record::id(id) AS rid, vector FROM memory WHERE record::id(id) IN $ids")
        .bind(("ids", id_strings))
        .await
        .map_err(|e| KbError::db("fetch_candidate_vectors", e))?;
    let rows: Vec<serde_json::Value> = res
        .take(0)
        .map_err(|e| KbError::db("deserializing candidate vectors", e))?;

    let mut out = HashMap::with_capacity(rows.len());
    for row in &rows {
        let rid = match row.get("rid").and_then(|v| v.as_str()) {
            Some(s) => MemoryId::from(s.to_string()),
            None => continue,
        };
        let vec_val = match row.get("vector") {
            Some(v) if !v.is_null() => v,
            _ => continue,
        };
        if let Ok(v) = serde_json::from_value::<Vec<f32>>(vec_val.clone()) {
            out.insert(rid, v);
        }
    }
    Ok(out)
}

/// Greedy MMR reordering of fused candidates.
///
/// Returns `(memories, scores)` reordered by MMR score. All input candidates
/// are returned (just reordered); `pool_out` caps the output length.
///
/// Candidates without a stored vector participate using relevance-only ordering —
/// they are appended after the MMR-selected set, preserving their fusion rank
/// relative to each other.
fn mmr_diversify(
    memories: Vec<MemorySummary>,
    fusion_scores: Vec<f64>,
    candidate_vectors: HashMap<MemoryId, Vec<f32>>,
    lambda: f64,
    pool_out: usize,
) -> (Vec<MemorySummary>, Vec<f64>) {
    let lambda = lambda.clamp(0.0, 1.0);
    if memories.len() <= 1 || candidate_vectors.is_empty() {
        return (memories, fusion_scores);
    }

    // MinMax-normalize fusion scores to [0, 1].
    let norm_rel = min_max_normalize(fusion_scores.iter().copied());

    // Partition: candidates with vectors (MMR-eligible) vs without (fallback).
    let mut with_vec: Vec<(usize, &MemorySummary, f64)> = memories
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            if candidate_vectors.contains_key(&m.id) {
                Some((i, m, norm_rel[i]))
            } else {
                None
            }
        })
        .collect();

    let without_vec: Vec<(usize, &MemorySummary, f64)> = memories
        .iter()
        .enumerate()
        .filter(|(i, m)| !candidate_vectors.contains_key(&m.id) && *i < norm_rel.len())
        .map(|(i, m)| (i, m, norm_rel[i]))
        .collect();

    if with_vec.is_empty() {
        // No vectors at all — return unchanged.
        return (memories, fusion_scores);
    }

    // Greedy MMR selection from the with-vector pool.
    let target = pool_out.min(with_vec.len());
    let mut selected_indices: Vec<usize> = Vec::with_capacity(target);
    // picked_vecs: cloned vecs for similarity calculation against future candidates.
    let mut picked_vecs: Vec<Vec<f32>> = Vec::with_capacity(target);

    // Track which original indices have been consumed.
    let mut remaining: Vec<(usize, &MemorySummary, f64)> = std::mem::take(&mut with_vec);

    while selected_indices.len() < target && !remaining.is_empty() {
        let best_pos = if picked_vecs.is_empty() {
            // First pick: highest normalized relevance.
            remaining
                .iter()
                .enumerate()
                .max_by(|(_, (_, _, ra)), (_, (_, _, rb))| {
                    ra.partial_cmp(rb).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(pos, _)| pos)
                .unwrap_or(0)
        } else {
            remaining
                .iter()
                .enumerate()
                .max_by(|(_, (_, m_a, rel_a)), (_, (_, m_b, rel_b))| {
                    let va = candidate_vectors.get(&m_a.id).map(|v| v.as_slice()).unwrap_or(&[]);
                    let vb = candidate_vectors.get(&m_b.id).map(|v| v.as_slice()).unwrap_or(&[]);
                    let max_sim_a = picked_vecs.iter().map(|pv| cosine_sim(va, pv)).fold(0.0f64, f64::max);
                    let max_sim_b = picked_vecs.iter().map(|pv| cosine_sim(vb, pv)).fold(0.0f64, f64::max);
                    let score_a = lambda * rel_a - (1.0 - lambda) * max_sim_a;
                    let score_b = lambda * rel_b - (1.0 - lambda) * max_sim_b;
                    score_a.partial_cmp(&score_b).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(pos, _)| pos)
                .unwrap_or(0)
        };

        let (orig_i, mem, _) = remaining.remove(best_pos);
        let vec = candidate_vectors.get(&mem.id).cloned().unwrap_or_default();
        picked_vecs.push(vec);
        selected_indices.push(orig_i);
    }

    // Append remaining with-vector candidates that didn't fit in the greedy selection
    // (preserving fusion order within them).
    for (orig_i, _, _) in &remaining {
        selected_indices.push(*orig_i);
    }

    // Append no-vector candidates (preserving fusion order).
    for (orig_i, _, _) in &without_vec {
        selected_indices.push(*orig_i);
    }

    selected_indices.truncate(pool_out);

    let out_memories: Vec<MemorySummary> = selected_indices.iter().map(|&i| memories[i].clone()).collect();
    let out_scores: Vec<f64> = selected_indices.iter().map(|&i| fusion_scores[i]).collect();
    (out_memories, out_scores)
}

// ── SM-2 spaced-repetition update ───────────────────────────────────────────

/// Compute SM-2 schedule updates from raw DB rows and fire them off
/// asynchronously. This runs in a background task so recall latency is
/// not affected.
fn spawn_sm2_access_update(db: SurrealClient, rows: &[serde_json::Value]) {
    let updates: Vec<Sm2Update> = rows.iter().filter_map(compute_sm2).collect();
    if updates.is_empty() {
        return;
    }

    tokio::spawn(async move {
        // Single round-trip: FOR loop with parameterized bind avoids N+1 and
        // satisfies the no-string-interpolation invariant.
        // serde_json::Value implements SurrealValue so it can cross the bind boundary.
        let updates_json = match serde_json::to_value(&updates) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("SM-2 serialization failed: {e}");
                return;
            }
        };
        let sql = "FOR $u IN $updates {
            UPDATE type::record('memory', $u.id)
            SET ease_factor     = $u.ease_factor,
                review_interval = $u.review_interval,
                next_review_at  = <datetime>$u.next_review_at;
        };";
        if let Err(e) = db.query(sql).bind(("updates", updates_json)).await {
            tracing::warn!("SM-2 batch update failed: {e}");
        }
    });
}

#[derive(serde::Serialize)]
struct Sm2Update {
    id: String,
    ease_factor: f64,
    review_interval: i64,
    next_review_at: DateTime<Utc>,
}

fn compute_sm2(row: &serde_json::Value) -> Option<Sm2Update> {
    let id = extract_id_from_row(row);
    if id.is_empty() {
        return None;
    }

    let ef = row
        .get("ease_factor")
        .and_then(|v| v.as_f64())
        .unwrap_or(2.5);
    let ri = row
        .get("review_interval")
        .and_then(|v| v.as_i64())
        .unwrap_or(1);

    // SM-2: successful recall → grow interval, increase ease slightly.
    // Cap at MAX_INTERVAL_DAYS before cast to prevent f64→i64 overflow UB on
    // long-lived memories with many review cycles.
    let new_ri = ((ri as f64) * ef)
        .ceil()
        .clamp(1.0, MAX_INTERVAL_DAYS) as i64;
    let new_ef = (ef + 0.1).min(4.0);
    let next_review = Utc::now() + Duration::days(new_ri);

    Some(Sm2Update {
        id,
        ease_factor: new_ef,
        review_interval: new_ri,
        next_review_at: next_review,
    })
}

// ── Spreading Activation ────────────────────────────────────────────────────

/// Fetch one-hop graph neighbors of the given memory IDs via extends, derives,
/// relates, AND involves edges. Applies ADR-023 per-edge-type weights and
/// hop decay so high-weight edges (derives, extends) surface before weak
/// auto-linked involves neighbors.
///
/// The involves traversal walks: memory→involves→entity→involves←memory,
/// discovering "other memories about the same person/entity."
///
/// Visibility matches the main recall contract: project-scoped queries allow
/// Private (visibility != 'sensitive'); cross-project queries restrict to Standard only.
/// The author filter is applied so spreading activation stays within the same author bucket.
///
/// Returns at most `cfg.cap` neighbors, sorted by descending weighted score.
/// Scores are computed internally; only MemorySummary values are returned.
async fn spreading_activation(
    db: &SurrealClient,
    known_ids: &[String],
    cfg: &SpreadingConfig,
    project: Option<&str>,
    author_filter: &AuthorFilter,
) -> Result<Vec<MemorySummary>> {
    if known_ids.is_empty() || cfg.cap == 0 {
        return Ok(vec![]);
    }

    let w = &cfg.edge_weights;
    // hop_depth=1 for single-hop; hop decay factor applied once.
    let hop = cfg.hop_decay;

    // Collect neighbor IDs from graph edges (both directions) with per-field weights.
    // Each field group maps to the ADR-023 edge weight for that edge type.
    // The involves traversal is handled separately (see hub-cap logic below).
    let mut result = db
        .query(
            "SELECT
                array::flatten(->extends->memory.id) AS ext_out,
                array::flatten(<-extends<-memory.id) AS ext_in,
                array::flatten(->derives->memory.id) AS der_out,
                array::flatten(<-derives<-memory.id) AS der_in,
                array::flatten(->relates->memory.id) AS rel_out,
                array::flatten(<-relates<-memory.id) AS rel_in,
                array::flatten(->updates->memory.id) AS upd_out,
                array::flatten(<-updates<-memory.id) AS upd_in
             FROM memory WHERE record::id(id) IN $ids",
        )
        .bind(("ids", known_ids.to_vec()))
        .await
        .map_err(|e| KbError::db("spreading activation neighbor query", e))?;

    let mut rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing spreading activation neighbors", e))?;

    // Hub-degree cap for involves edges (ADR-029, Change 1).
    // Two-query approach: get entity IDs reachable via involves, check each
    // entity's degree, skip entities with degree > involves_hub_cap.
    // Entities reached via derives/extends/updates/relates are NOT affected.
    let hub_cap = w.involves_hub_cap;
    let inv_neighbor_ids: Vec<String> = {
        let mut inv_result = db
            .query(
                "SELECT array::flatten(->involves->entity.id) AS ent_ids
                 FROM memory WHERE record::id(id) IN $ids",
            )
            .bind(("ids", known_ids.to_vec()))
            .await
            .map_err(|e| KbError::db("spreading activation involves entity query", e))?;
        let inv_ent_rows: Vec<serde_json::Value> = inv_result
            .take(0)
            .map_err(|e| KbError::db("deserializing involves entity ids", e))?;

        // Collect flat list of entity record IDs reachable via involves.
        let ent_ids: Vec<surrealdb::types::RecordId> = inv_ent_rows
            .iter()
            .flat_map(|row| {
                row.get("ent_ids")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| match v {
                                serde_json::Value::String(s) => {
                                    let id_part = s.strip_prefix("entity:").unwrap_or(s);
                                    Some(surrealdb::types::RecordId::new("entity", id_part))
                                }
                                serde_json::Value::Object(obj) => {
                                    // Nested {tb, id} representation from SurrealDB.
                                    let id_part = obj
                                        .get("id")
                                        .and_then(|inner| match inner {
                                            serde_json::Value::String(s) => Some(s.as_str()),
                                            _ => None,
                                        })
                                        .unwrap_or("");
                                    if id_part.is_empty() {
                                        None
                                    } else {
                                        Some(surrealdb::types::RecordId::new("entity", id_part))
                                    }
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .collect();

        if ent_ids.is_empty() {
            vec![]
        } else {
            // Degree check: count how many memories each entity is involved with.
            let mut deg_result = db
                .query(
                    "SELECT record::id(id) AS eid, count(<-involves) AS deg
                     FROM entity WHERE id INSIDE $eids",
                )
                .bind(("eids", ent_ids))
                .await
                .map_err(|e| KbError::db("spreading activation entity degree query", e))?;
            let deg_rows: Vec<serde_json::Value> = deg_result
                .take(0)
                .map_err(|e| KbError::db("deserializing entity degrees", e))?;

            // Collect non-hub entity IDs (degree <= hub_cap).
            let non_hub_eids: Vec<surrealdb::types::RecordId> = deg_rows
                .iter()
                .filter_map(|row| {
                    let deg = row.get("deg").and_then(|v| v.as_u64()).unwrap_or(0);
                    if deg <= hub_cap as u64 {
                        row.get("eid").and_then(|v| v.as_str()).map(|s| {
                            surrealdb::types::RecordId::new("entity", s)
                        })
                    } else {
                        None
                    }
                })
                .collect();

            if non_hub_eids.is_empty() {
                vec![]
            } else {
                // Get memory IDs reachable from non-hub entities.
                let mut mem_result = db
                    .query(
                        "SELECT array::flatten(<-involves<-memory.id) AS mem_ids
                         FROM entity WHERE id INSIDE $eids",
                    )
                    .bind(("eids", non_hub_eids))
                    .await
                    .map_err(|e| KbError::db("spreading activation involves memory query", e))?;
                let mem_rows: Vec<serde_json::Value> = mem_result
                    .take(0)
                    .map_err(|e| KbError::db("deserializing involves memory ids", e))?;

                mem_rows
                    .iter()
                    .flat_map(|row| {
                        row.get("mem_ids")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| {
                                        let s = match v {
                                            serde_json::Value::String(s) => s
                                                .strip_prefix("memory:")
                                                .unwrap_or(s)
                                                .to_string(),
                                            serde_json::Value::Object(obj) => obj
                                                .get("id")
                                                .and_then(|inner| match inner {
                                                    serde_json::Value::String(s) => Some(s.clone()),
                                                    serde_json::Value::Object(o) => o
                                                        .get("String")
                                                        .and_then(|s| s.as_str().map(String::from)),
                                                    _ => None,
                                                })
                                                .unwrap_or_default(),
                                            _ => return None,
                                        };
                                        if s.is_empty() { None } else { Some(s) }
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            }
        }
    };

    // Inject inv_via_entity into each row as a synthetic field so the existing
    // scoring loop (field_weights) can process it uniformly. We attach the full
    // list once to the first row (or a synthetic row) since hub-filtered IDs are
    // not per-source-memory but an aggregate across all primary memories.
    // To keep the scoring loop unchanged, we add a synthetic row for involves.
    let synthetic_inv_row = serde_json::json!({"inv_via_entity": inv_neighbor_ids});
    rows.push(synthetic_inv_row);

    // Collect typed_relation neighbors with confidence so we can compute
    // the variable weight (confidence × typed_relation_max).
    // Edge-field filtering (retracted_at, confidence) is done in WHERE on the edge
    // table rather than the graph traversal bracket — bracket-filter only applies
    // to target nodes, not edge records.
    let rec_ids: Vec<surrealdb::types::RecordId> = known_ids
        .iter()
        .map(|id| surrealdb::types::RecordId::new("memory", id.as_str()))
        .collect();
    let (tr_out_rows, tr_in_rows): (Vec<serde_json::Value>, Vec<serde_json::Value>) = match db
        .query(
            "SELECT out AS tr_out, confidence AS tr_conf FROM typed_relation
             WHERE in INSIDE $rec_ids
               AND retracted_at IS NONE
               AND confidence >= $conf_floor;
             SELECT in AS tr_in, confidence AS tr_conf FROM typed_relation
             WHERE out INSIDE $rec_ids
               AND retracted_at IS NONE
               AND confidence >= $conf_floor;",
        )
        .bind(("rec_ids", rec_ids))
        .bind(("conf_floor", w.typed_relation_confidence_floor))
        .await
    {
        Ok(mut r) => {
            let out: Vec<serde_json::Value> = r.take(0).unwrap_or_default();
            let inn: Vec<serde_json::Value> = r.take(1).unwrap_or_default();
            (out, inn)
        }
        Err(e) => {
            tracing::warn!("typed_relation neighbor query failed, skipping: {e}");
            (vec![], vec![])
        }
    };

    let known_set: HashSet<&str> = known_ids.iter().map(|s| s.as_str()).collect();

    // neighbor_scores: id → best weighted score seen so far (dedup with max).
    // score = parent_score(1.0) × edge_weight × hop_decay^1
    let mut neighbor_scores: HashMap<String, f64> = HashMap::new();

    let extract_ids_from_field = |row: &serde_json::Value, field: &str| -> Vec<String> {
        row.get(field)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        let s = match v {
                            serde_json::Value::String(s) => {
                                s.strip_prefix("memory:").unwrap_or(s).to_string()
                            }
                            serde_json::Value::Object(obj) => obj
                                .get("id")
                                .and_then(|inner| match inner {
                                    serde_json::Value::String(s) => Some(s.clone()),
                                    serde_json::Value::Object(o) => {
                                        o.get("String").and_then(|s| s.as_str().map(String::from))
                                    }
                                    _ => None,
                                })
                                .unwrap_or_default(),
                            _ => return None,
                        };
                        if s.is_empty() { None } else { Some(s) }
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    // Field → edge weight mapping (ADR-023 table).
    let field_weights: &[(&str, f64)] = &[
        ("der_out", w.derives),
        ("der_in", w.derives),
        ("ext_out", w.extends),
        ("ext_in", w.extends),
        ("upd_out", w.updates),
        ("upd_in", w.updates),
        ("rel_out", w.relates),
        ("rel_in", w.relates),
        ("inv_via_entity", w.involves),
    ];

    for row in &rows {
        for (field, edge_weight) in field_weights {
            let score = edge_weight * hop;
            for id in extract_ids_from_field(row, field) {
                if !known_set.contains(id.as_str()) {
                    let entry = neighbor_scores.entry(id).or_insert(0.0);
                    if score > *entry {
                        *entry = score;
                    }
                }
            }
        }
    }

    // Extract typed_relation neighbors: each row has scalar record ID + confidence.
    let extract_scalar_record_id = |row: &serde_json::Value, field: &str| -> Option<String> {
        let v = row.get(field)?;
        let s = match v {
            serde_json::Value::String(s) => s.strip_prefix("memory:").unwrap_or(s).to_string(),
            serde_json::Value::Object(obj) => obj
                .get("id")
                .and_then(|inner| match inner {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Object(o) => {
                        o.get("String").and_then(|s| s.as_str().map(String::from))
                    }
                    _ => None,
                })
                .unwrap_or_default(),
            _ => return None,
        };
        if s.is_empty() { None } else { Some(s) }
    };

    let extract_confidence = |row: &serde_json::Value| -> f64 {
        row.get("tr_conf")
            .and_then(|v| v.as_f64())
            .unwrap_or(w.typed_relation_confidence_floor)
    };

    for row in &tr_out_rows {
        if let Some(id) = extract_scalar_record_id(row, "tr_out")
            && !known_set.contains(id.as_str())
        {
            let confidence = extract_confidence(row);
            let score = confidence * w.typed_relation_max * hop;
            let entry = neighbor_scores.entry(id).or_insert(0.0);
            if score > *entry {
                *entry = score;
            }
        }
    }
    for row in &tr_in_rows {
        if let Some(id) = extract_scalar_record_id(row, "tr_in")
            && !known_set.contains(id.as_str())
        {
            let confidence = extract_confidence(row);
            let score = confidence * w.typed_relation_max * hop;
            let entry = neighbor_scores.entry(id).or_insert(0.0);
            if score > *entry {
                *entry = score;
            }
        }
    }

    let candidates_seen = neighbor_scores.len() as u64;
    if neighbor_scores.is_empty() {
        recall_metrics::record_sa(recall_metrics::SpreadingActivationStats {
            candidates_seen: 0,
            neighbors_added: 0,
            cap_saturated: false,
        });
        return Ok(vec![]);
    }

    // Sort by score descending, take top cap, then fetch memory records.
    let mut scored: Vec<(String, f64)> = neighbor_scores.into_iter().collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(cfg.cap);
    let neighbors_added = scored.len() as u64;
    let cap_saturated = candidates_seen > neighbors_added && neighbors_added >= cfg.cap as u64;
    tracing::debug!(candidates_seen, neighbors_added, cap_saturated, "spreading activation summary");
    recall_metrics::record_sa(recall_metrics::SpreadingActivationStats { candidates_seen, neighbors_added, cap_saturated });
    let neighbor_ids: Vec<String> = scored.into_iter().map(|(id, _)| id).collect();

    // Fetch neighbor memory records applying the same visibility contract as the
    // main recall query: project-scoped allows Private (visibility != 'sensitive');
    // cross-project restricts to Standard only to prevent cross-project private leaks.
    // Project filter is also applied so neighbors stay within the query's scope.
    let neighbor_limit = cfg.cap as i64;
    let (visibility_clause, project_clause) = if project.is_some() {
        (
            "AND visibility != 'sensitive'",
            "AND project = $project",
        )
    } else {
        ("AND visibility = 'standard'", "")
    };
    let author_clause = match author_filter {
        AuthorFilter::None => "",
        AuthorFilter::UserOnly => " AND author IS NONE",
        AuthorFilter::Agent(_) => " AND author = $neighbor_author_id",
    };
    let neighbor_sql = format!(
        "SELECT * OMIT vector FROM memory
         WHERE {BITEMPORAL_ACTIVE}
           {visibility_clause}
           {project_clause}
           {author_clause}
           AND record::id(id) IN $ids
         LIMIT $limit"
    );
    let mut neighbor_q = db.query(&neighbor_sql);
    neighbor_q = neighbor_q.bind(("ids", neighbor_ids));
    neighbor_q = neighbor_q.bind(("limit", neighbor_limit));
    if let Some(p) = project {
        neighbor_q = neighbor_q.bind(("project", p.to_string()));
    }
    if let AuthorFilter::Agent(aid) = author_filter {
        let rid = RecordId::new("agent", aid.as_str());
        neighbor_q = neighbor_q.bind(("neighbor_author_id", rid));
    }
    let mut result = neighbor_q
        .await
        .map_err(|e| KbError::db("spreading activation fetch neighbors", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing spreading activation results", e))?;

    let mut memories = Vec::new();
    for row in rows {
        let id_str = extract_id_from_row(&row);
        let id = MemoryId::from(id_str);
        match parse_memory_row(row, &id) {
            Ok(mem) => memories.push(MemorySummary::from(mem)),
            Err(e) => {
                tracing::warn!("skipping malformed neighbor row: {e}");
            }
        }
    }
    Ok(memories)
}

// ── Entity-Anchored Detection ───────────────────────────────────────────────

/// Check if this query should use entity-anchored strategy (larger BM25 fetch).
fn is_entity_anchored(person: Option<&str>) -> bool {
    person.is_some()
}

/// Upper bound on the per-source candidate pool.
const MAX_POOL: usize = 200;

/// Effective candidate pool: `max(limit, pool_size)`, capped at [`MAX_POOL`].
fn pool_size(limit: u8, fusion_cfg: &FusionConfig) -> usize {
    (limit as usize).max(fusion_cfg.pool_size).min(MAX_POOL)
}

// ── Adaptive reranker gate (ADR-022) ────────────────────────────────────────

/// Decide whether to invoke the cross-encoder reranker for this query.
///
/// Force conditions take precedence over skip conditions.
/// `caller_override` always wins over all heuristics (explicit beats policy).
fn should_rerank(
    query: &str,
    rrf_scores: &[f64],
    person: Option<&str>,
    memory_type: Option<MemoryType>,
    caller_override: Option<bool>,
    policy: RerankerPolicy,
) -> bool {
    match policy {
        RerankerPolicy::Always => return true,
        RerankerPolicy::Never => return false,
        RerankerPolicy::Adaptive => {}
    }

    // Explicit caller beats all heuristics.
    if let Some(forced) = caller_override {
        return forced;
    }

    if rrf_scores.is_empty() {
        return false;
    }

    // Force: person filter — name disambiguation benefits most from cross-encoder.
    if person.is_some() {
        return true;
    }

    let word_count = query.split_whitespace().count();

    // Force: long query — semantic nuance is high; cross-encoder excels.
    if word_count > 6 {
        return true;
    }

    // Skip: dominant top-1 — cross-encoder cannot improve position 0.
    if rrf_scores.len() >= 2 && rrf_scores[0] > 2.0 * rrf_scores[1] {
        return false;
    }

    // Skip: ephemeral types — recall is lookup, not ranking.
    if matches!(memory_type, Some(MemoryType::Working)) {
        return false;
    }

    // Skip: short lookup with no disambiguation need.
    if word_count <= 3 {
        return false;
    }

    // Default: 4-6 word query of any non-working type — rerank. The old
    // Semantic-only fallthrough left episodic/procedural/prospective queries
    // in raw fusion order, where Recall@1 is weakest.
    true
}

// ── Primary Recall ──────────────────────────────────────────────────────────

/// Hybrid recall: BM25 full-text + optional vector similarity, fused with RRF.
///
/// Pipeline:
/// 0. Detect query intent (auto-route strategy)
/// 1. Run BM25 query (always)
/// 2. Run vector cosine similarity query (if `query_vector` provided)
/// 3. Fuse results via Reciprocal Rank Fusion (weights adjusted by intent)
/// 4. Entity-anchored person filter (if person specified)
/// 5. Spreading activation: expand one hop via graph edges (includes involves)
/// 6. SM-2 access update (fire-and-forget background task)
/// 7. Lost-in-the-Middle context reordering
#[allow(clippy::too_many_arguments)]
#[instrument(skip(db, query, query_vector, reranker, spreading_cfg))]
pub async fn recall(
    db: &SurrealClient,
    query: &str,
    query_vector: Option<Vec<f32>>,
    reranker: Option<&Reranker>,
    memory_type: Option<MemoryType>,
    project: Option<&str>,
    domain: Option<&str>,
    person: Option<&str>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    channel: Option<Channel>,
    modality: Option<Modality>,
    limit: u8,
    author_filter: &AuthorFilter,
    min_score: f64,
    spreading_cfg: &SpreadingConfig,
    fusion_cfg: &FusionConfig,
    policy: RerankerPolicy,
    caller_override: Option<bool>,
    spreading_override: Option<bool>,
    mmr_cfg: &MmrConfig,
) -> Result<Vec<MemorySummary>> {
    // Candidate pool: fetch and fuse more than the caller asked for so MMR and
    // rerank can promote evidence ranked below `limit`; truncate afterwards.
    let pool = pool_size(limit, fusion_cfg);
    let fetch_limit = pool as i64;

    // For entity-anchored queries, increase BM25 fetch to ensure we have
    // enough candidates before the entity intersection in Step 4.
    let bm25_fetch = if is_entity_anchored(person) {
        (fetch_limit * 2).min(MAX_POOL as i64 * 2)
    } else {
        fetch_limit
    };

    // ── Step 1: BM25 full-text search ───────────────────────────────────

    let _bm25_started = Instant::now();
    let mut conditions = vec![BITEMPORAL_ACTIVE.to_string()];

    // Visibility filtering: prevent private/sensitive memories from leaking.
    // - Project-scoped: exclude sensitive only (private is OK within the project)
    // - Cross-project (no project filter): exclude both private and sensitive
    if project.is_some() {
        conditions.push("visibility != 'sensitive'".to_string());
    } else {
        conditions.push("visibility = 'standard'".to_string());
    }

    if memory_type.is_some() {
        conditions.push("memory_type = $memory_type".to_string());
    }
    if project.is_some() {
        conditions.push("project = $project".to_string());
    }
    if domain.is_some() {
        conditions.push("$domain INSIDE domain".to_string());
    }
    if since.is_some() {
        conditions.push("event_at >= $since".to_string());
    }
    if until.is_some() {
        conditions.push("event_at <= $until".to_string());
    }
    if channel.is_some() {
        conditions.push("channel = $channel".to_string());
    }
    if modality.is_some() {
        conditions.push("modality = $modality".to_string());
    }
    match author_filter {
        AuthorFilter::None => {}
        AuthorFilter::UserOnly => conditions.push("author IS NONE".to_string()),
        AuthorFilter::Agent(_) => conditions.push("author = $bm25_author_id".to_string()),
    }
    let where_clause = conditions.join(" AND ");

    let bm25_sql = if query.trim().is_empty() {
        format!(
            "SELECT *, 0.0 AS _bm25_score OMIT vector FROM memory
             WHERE {where_clause}
             ORDER BY importance DESC, event_at DESC
             LIMIT $limit"
        )
    } else {
        format!(
            "SELECT *, ((search::score(0) ?? 0) + (search::score(1) ?? 0) + (search::score(2) ?? 0)) AS _bm25_score OMIT vector FROM memory
             WHERE ({where_clause})
               AND (summary @0@ $query OR content @1@ $query OR anchor_entities @2@ $query)
             ORDER BY _bm25_score DESC, importance DESC
             LIMIT $limit"
        )
    };

    let mut bm25_q = db.query(&bm25_sql);
    bm25_q = bm25_q.bind(("query", query.to_string()));
    bm25_q = bm25_q.bind(("limit", bm25_fetch));
    if let Some(mt) = memory_type {
        bm25_q = bm25_q.bind(("memory_type", enum_to_value(mt)?));
    }
    if let Some(p) = project {
        bm25_q = bm25_q.bind(("project", p.to_string()));
    }
    if let Some(d) = domain {
        bm25_q = bm25_q.bind(("domain", d.to_string()));
    }
    if let Some(s) = since {
        bm25_q = bm25_q.bind(("since", s));
    }
    if let Some(u) = until {
        bm25_q = bm25_q.bind(("until", u));
    }
    bm25_q = bm25_q.bind(("channel", channel.map(|c| c.to_string())));
    bm25_q = bm25_q.bind(("modality", modality.map(|m| m.to_string())));
    if let AuthorFilter::Agent(aid) = author_filter {
        let rid = RecordId::new("agent", aid.as_str());
        bm25_q = bm25_q.bind(("bm25_author_id", rid));
    }

    let mut bm25_result = bm25_q
        .await
        .map_err(|e| KbError::db("BM25 recall query", e))?;
    let bm25_rows: Vec<serde_json::Value> = bm25_result
        .take(0)
        .map_err(|e| KbError::db("deserializing BM25 recall results", e))?;

    let bm25_raw_scores = extract_scores(&bm25_rows);
    let bm25_memories = parse_rows(&bm25_rows);
    // Build content map from BM25 rows (SELECT * includes content). Extended
    // below with vector rows so all candidates can be paired for reranking.
    let mut content_map = build_content_map(&bm25_rows);
    recall_metrics::record(RecallStage::Bm25, _bm25_started.elapsed());

    // ── Step 2: Vector similarity search (optional) ─────────────────────

    let _vector_started = Instant::now();
    let _vector_qv_present = query_vector.is_some();
    let (vector_memories, vec_raw_scores) = if let Some(qv) = query_vector {
        // Brute-force cosine similarity scan. SurrealKV embedded doesn't support
        // MTREE ANN indexes yet, but brute-force is fast enough for a personal KB
        // with <100k records.
        let visibility_filter = if project.is_some() {
            " AND visibility != 'sensitive'"
        } else {
            " AND visibility = 'standard'"
        };
        let author_filter_str = match author_filter {
            AuthorFilter::None => "",
            AuthorFilter::UserOnly => " AND author IS NONE",
            AuthorFilter::Agent(_) => " AND author = $vec_author_id",
        };
        // Over-fetch: KNN candidates are filtered by the predicates below afterwards.
        let knn = crate::db::vector_index::candidate_predicate(fetch_limit as usize * 4);
        let vector_sql = format!(
            "SELECT *, vector::similarity::cosine(vector, $qv) AS _vscore OMIT vector FROM memory
             WHERE {BITEMPORAL_ACTIVE} AND {knn}{visibility_filter}{type_filter}{project_filter}{domain_filter}{since_filter}{until_filter}{channel_filter}{modality_filter}{author_filter_str}
             ORDER BY _vscore DESC
             LIMIT $limit",
            type_filter = if memory_type.is_some() { " AND memory_type = $memory_type" } else { "" },
            project_filter = if project.is_some() { " AND project = $project" } else { "" },
            domain_filter = if domain.is_some() { " AND $domain INSIDE domain" } else { "" },
            since_filter = if since.is_some() { " AND event_at >= $since" } else { "" },
            until_filter = if until.is_some() { " AND event_at <= $until" } else { "" },
            channel_filter = if channel.is_some() { " AND channel = $channel" } else { "" },
            modality_filter = if modality.is_some() { " AND modality = $modality" } else { "" },
        );

        let mut vec_q = db.query(&vector_sql);
        vec_q = vec_q.bind(("qv", qv));
        vec_q = vec_q.bind(("limit", fetch_limit));
        if let Some(mt) = memory_type {
            vec_q = vec_q.bind(("memory_type", enum_to_value(mt)?));
        }
        if let Some(p) = project {
            vec_q = vec_q.bind(("project", p.to_string()));
        }
        if let Some(d) = domain {
            vec_q = vec_q.bind(("domain", d.to_string()));
        }
        if let Some(s) = since {
            vec_q = vec_q.bind(("since", s));
        }
        if let Some(u) = until {
            vec_q = vec_q.bind(("until", u));
        }
        vec_q = vec_q.bind(("channel", channel.map(|c| c.to_string())));
        vec_q = vec_q.bind(("modality", modality.map(|m| m.to_string())));
        if let AuthorFilter::Agent(aid) = author_filter {
            let rid = RecordId::new("agent", aid.as_str());
            vec_q = vec_q.bind(("vec_author_id", rid));
        }

        match vec_q.await {
            Ok(mut res) => {
                let rows: Vec<serde_json::Value> = match res.take(0) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("deserializing vector search results: {e}");
                        vec![]
                    }
                };
                content_map.extend(build_content_map(&rows));
                let vscores = extract_vscores(&rows);
                (parse_rows(&rows), vscores)
            }
            Err(e) => {
                // Vector search is best-effort — fall back to BM25 only.
                tracing::warn!("vector search failed, using BM25 only: {e}");
                (vec![], vec![])
            }
        }
    } else {
        (vec![], vec![])
    };
    if _vector_qv_present {
        recall_metrics::record(RecallStage::Vector, _vector_started.elapsed());
    }

    // ── Step 3: Fusion (RRF or Convex Combination) ────────────────────
    // Strategy is selected per search.fusion.strategy in config.

    let _fusion_started = Instant::now();
    let limit_usize = limit.min(50) as usize;
    // Reserve slots for spreading activation when limit is generous enough
    // (`[search.spreading].reserve_slots`). At small limits, don't sacrifice
    // primary recall for graph hops.
    let primary_cap = if limit_usize > 10 {
        limit_usize.saturating_sub(spreading_cfg.reserve_slots)
    } else {
        limit_usize
    };
    // Fusion keeps the whole pool; results are cut to `primary_cap` after rerank.
    let fused_cap = pool.max(primary_cap);
    let (mut memories, rrf_scores) = if vector_memories.is_empty() {
        // No vector signal — RRF score for a single source at rank r is 1/(K+r+1).
        // Apply min_score gate manually so callers see consistent semantics
        // whether or not the embedding path was available.
        let ranked: Vec<MemorySummary> = bm25_memories
            .into_iter()
            .enumerate()
            .filter(|(rank, _)| rrf_score(*rank) >= min_score)
            .take(fused_cap)
            .map(|(_, m)| m)
            .collect();
        let scores: Vec<f64> = (0..ranked.len()).map(rrf_score).collect();
        (ranked, scores)
    } else {
        match fusion_cfg.strategy {
            FusionStrategy::Rrf => rrf_fuse(&bm25_memories, &vector_memories, fused_cap, min_score),
            FusionStrategy::ConvexCombination => convex_fuse(
                &bm25_memories,
                &bm25_raw_scores,
                &vector_memories,
                &vec_raw_scores,
                fusion_cfg.vector_weight,
                fused_cap,
                fusion_cfg.min_score,
            ),
        }
    };
    recall_metrics::record(RecallStage::Fusion, _fusion_started.elapsed());

    // ── Step 3.4: MMR diversification (ADR-031, post-fusion) ─────────
    // Skipped when: disabled, BM25-only path (no vectors), ≤1 candidate,
    // or query_vector was None (no embedding signal — _vector_qv_present tracks this).
    let mut rrf_scores = rrf_scores;
    if mmr_cfg.enabled
        && !vector_memories.is_empty()
        && memories.len() > 1
        && _vector_qv_present
    {
        let cand_ids: Vec<MemoryId> = memories.iter().map(|m| m.id.clone()).collect();
        let candidate_vectors = fetch_candidate_vectors(db, &cand_ids)
            .await
            .unwrap_or_default(); // soft-fail: empty map → MMR no-op
        let mmr_start = Instant::now();
        let pool_out = memories.len();
        let (m_new, s_new) =
            mmr_diversify(memories, rrf_scores, candidate_vectors, mmr_cfg.lambda, pool_out);
        memories = m_new;
        rrf_scores = s_new;
        recall_metrics::record(RecallStage::Mmr, mmr_start.elapsed());
    }

    // ── Step 3.5: Cross-encoder rerank (ADR-017, ADR-022) ────────────
    if let Some(r) = reranker
        && !memories.is_empty()
        && should_rerank(
            query,
            &rrf_scores,
            person,
            memory_type,
            caller_override,
            policy,
        )
    {
        tracing::info!(
            target: "rerank.decision",
            policy = ?policy,
            decision = true,
            "rerank gate"
        );
        let _rerank_started = Instant::now();
        let candidates = to_rerank_candidates(memories, &content_map);
        memories = r.rerank(query, candidates).await;
        recall_metrics::record(RecallStage::Rerank, _rerank_started.elapsed());
    } else if reranker.is_some() {
        tracing::info!(
            target: "rerank.decision",
            policy = ?policy,
            decision = false,
            "rerank gate"
        );
    }

    // Without a person filter the pool is cut to the result cap here; the person
    // filter below intersects against the full pool and truncates itself.
    if person.is_none() {
        memories.truncate(primary_cap);
    }

    // ── Step 4: Entity-anchored person filter ─────────────────────────
    //
    // When a person filter is present, we start from the entity's `involves`
    // edges to collect ALL memory IDs for that person, then intersect with
    // text-ranked results. This avoids the old post-hoc bug where valid
    // results ranked beyond the BM25 limit were dropped.

    if let Some(person_name) = person {
        // Exact lowercased match prevents "Alex" from matching "Alexander".
        // Names are stored as supplied at write time; we normalize both sides.
        //
        // Fix (ADR-029): also fetch degree so hub entities (degree > involves_hub_cap)
        // can be skipped — same cap pattern as spreading_activation.
        // Fix: entity query gates on archived_at IS NONE to exclude soft-deleted entities.
        // Note: person table has no archived_at field (legacy table, no new writes since
        // graph.rs migration); hub cap applies to person rows via degree count as well.
        //
        // ADR-030: also resolve aliases. Canonical entity IDs from alias lookup are
        // added to the entity query via a second SQL statement.
        //
        // All statements share the same $name binding and execute in one round-trip.
        let hub_cap = spreading_cfg.edge_weights.involves_hub_cap as u64;

        // Resolve alias first (cheap — exact index lookup).
        let alias_entity_ids = entity_alias::resolve(db, person_name).await.unwrap_or_default();
        if alias_entity_ids.len() > 1 {
            warn!(
                person = person_name,
                count = alias_entity_ids.len(),
                "person filter: alias resolved to multiple canonical entities, including all"
            );
        }

        // Build a list of SurrealDB RecordIds for the alias-resolved entities so we
        // can query their involves-edges in the same round-trip.
        let alias_rids: Vec<surrealdb::types::RecordId> = alias_entity_ids
            .iter()
            .map(|eid| surrealdb::types::RecordId::new("entity", eid.as_str()))
            .collect();

        let mut combined_result = db
            .query(
                "SELECT array::flatten(<-involves<-memory.id) AS mem_ids, count(<-involves) AS degree FROM entity
                 WHERE string::lowercase(name) = string::lowercase($name) AND archived_at IS NONE;
                 SELECT array::flatten(<-involves<-memory.id) AS mem_ids, count(<-involves) AS degree FROM person
                 WHERE string::lowercase(name) = string::lowercase($name);
                 SELECT array::flatten(<-involves<-memory.id) AS mem_ids, count(<-involves) AS degree FROM entity
                 WHERE id INSIDE $alias_eids AND archived_at IS NONE",
            )
            .bind(("name", person_name.to_string()))
            .bind(("alias_eids", alias_rids))
            .await
            .map_err(|e| KbError::db("entity-anchored query", e))?;

        let mut entity_mem_ids = HashSet::new();

        let extract_ids_from_row = |row: &serde_json::Value| -> Vec<String> {
            row.get("mem_ids")
                .and_then(|m| m.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| {
                            v.as_str()
                                .map(|s| s.strip_prefix("memory:").unwrap_or(s).to_string())
                        })
                        .collect()
                })
                .unwrap_or_default()
        };

        // Apply hub cap: skip entity/person nodes whose involves-degree exceeds the cap.
        // Prevents corpus-wide hub entities from flooding the person filter with
        // uncorrelated memories (ADR-029, Change 1).
        let under_hub_cap = |row: &serde_json::Value| -> bool {
            let deg = row.get("degree").and_then(|v| v.as_u64()).unwrap_or(0);
            deg <= hub_cap
        };

        let entity_rows: Vec<serde_json::Value> = combined_result
            .take(0)
            .map_err(|e| KbError::db("deserializing entity filter", e))?;
        let person_rows: Vec<serde_json::Value> = combined_result
            .take(1)
            .map_err(|e| KbError::db("deserializing person filter", e))?;
        let alias_entity_rows: Vec<serde_json::Value> = combined_result
            .take(2)
            .map_err(|e| KbError::db("deserializing alias entity filter", e))?;

        for row in &entity_rows {
            if under_hub_cap(row) {
                for id in extract_ids_from_row(row) {
                    entity_mem_ids.insert(id);
                }
            }
        }
        for row in &person_rows {
            if under_hub_cap(row) {
                for id in extract_ids_from_row(row) {
                    entity_mem_ids.insert(id);
                }
            }
        }
        for row in &alias_entity_rows {
            if under_hub_cap(row) {
                for id in extract_ids_from_row(row) {
                    entity_mem_ids.insert(id);
                }
            }
        }

        if entity_mem_ids.is_empty() {
            memories.clear();
        } else {
            // Project-pivot: 2-hop probes ("config X in same project as person Y's
            // ticket") need answer memories that share the person's project even
            // when they don't directly involve the person. Pull the DISTINCT set
            // of projects from the person's involves-set; allow text-ranked
            // results whose project belongs to that set.
            let entity_id_vec: Vec<String> = entity_mem_ids.iter().cloned().collect();
            let mut person_projects: HashSet<String> = HashSet::new();
            let mut proj_result = db
                .query(
                    "SELECT project FROM memory
                     WHERE record::id(id) IN $ids AND project IS NOT NONE
                     GROUP BY project",
                )
                .bind(("ids", entity_id_vec))
                .await
                .map_err(|e| KbError::db("entity-projects query", e))?;
            let proj_rows: Vec<serde_json::Value> = proj_result
                .take(0)
                .map_err(|e| KbError::db("deserializing entity projects", e))?;
            for row in &proj_rows {
                if let Some(p) = row.get("project").and_then(|v| v.as_str()) {
                    person_projects.insert(p.to_string());
                }
            }

            // Keep text-ranked results that either involve the person directly OR
            // share one of the person's projects (project-pivot for 2-hop).
            let text_ranked: Vec<MemorySummary> = memories
                .iter()
                .filter(|m| {
                    entity_mem_ids.contains(m.id.as_str())
                        || m.project
                            .as_deref()
                            .is_some_and(|p| person_projects.contains(p))
                })
                .cloned()
                .collect();

            // Also fetch entity-linked memories NOT in text results (the fix:
            // these would have been dropped by post-hoc filtering before)
            let text_ids: HashSet<String> =
                text_ranked.iter().map(|m| m.id.as_str().to_string()).collect();
            let missing_ids: Vec<String> = entity_mem_ids
                .into_iter()
                .filter(|id| !text_ids.contains(id))
                .collect();

            let mut extra_memories = Vec::new();
            if !missing_ids.is_empty() {
                let visibility_filter = if project.is_some() {
                    "AND visibility != 'sensitive'"
                } else {
                    "AND visibility = 'standard'"
                };
                let entity_author_clause = match author_filter {
                    AuthorFilter::None => "",
                    AuthorFilter::UserOnly => "AND author IS NONE",
                    AuthorFilter::Agent(_) => "AND author = $entity_author_id",
                };
                let sql = format!(
                    "SELECT * OMIT vector FROM memory
                     WHERE {BITEMPORAL_ACTIVE} {visibility_filter}
                       {entity_author_clause}
                       AND record::id(id) IN $ids
                     ORDER BY importance DESC, event_at DESC
                     LIMIT $limit"
                );
                let extra_limit = i64::try_from(limit_usize.saturating_sub(text_ranked.len()))
                    .unwrap_or(i64::MAX);
                if extra_limit > 0 {
                    let mut q = db.query(&sql);
                    q = q.bind(("ids", missing_ids));
                    q = q.bind(("limit", extra_limit));
                    if let AuthorFilter::Agent(aid) = author_filter {
                        let rid = RecordId::new("agent", aid.as_str());
                        q = q.bind(("entity_author_id", rid));
                    }
                    if let Ok(mut res) = q.await {
                        let rows: Vec<serde_json::Value> = match res.take(0) {
                            Ok(r) => r,
                            Err(e) => {
                                tracing::warn!("deserializing entity extra memories: {e}");
                                vec![]
                            }
                        };
                        extra_memories = parse_rows(&rows);
                    }
                }
            }

            memories = text_ranked;
            memories.extend(extra_memories);
            memories.truncate(primary_cap);
        }
    }
    // Person paths that resolve no entity skip the truncate above.
    memories.truncate(primary_cap);

    // ── Step 4.5: Alias FTS pre-expansion (ADR-030) ─────────────────────
    //
    // When no explicit person= filter is present, run a single alias FTS lookup
    // on the query string.  Any matched canonical entity IDs are used to fetch
    // entity-linked memories and merge them into the primary results, giving
    // alias-anchored memories a chance to surface before spreading activation.
    //
    // Bounded at 5 entities / 5 extra memories — cheap, single FTS.
    // Only runs when there is still budget in the result list.
    if person.is_none() && !query.trim().is_empty() && memories.len() < primary_cap {
        let alias_anchor_ids = entity_alias::resolve_fts_warn_multi(db, query, 5).await;
        if !alias_anchor_ids.is_empty() {
            let alias_rids: Vec<surrealdb::types::RecordId> = alias_anchor_ids
                .iter()
                .map(|eid| surrealdb::types::RecordId::new("entity", eid.as_str()))
                .collect();
            let existing_ids: HashSet<String> =
                memories.iter().map(|m| m.id.as_str().to_string()).collect();

            let visibility_clause = if project.is_some() {
                "AND visibility != 'sensitive'"
            } else {
                "AND visibility = 'standard'"
            };
            let proj_clause = if project.is_some() { "AND project = $aa_project" } else { "" };
            let author_clause_aa = match author_filter {
                AuthorFilter::None => "",
                AuthorFilter::UserOnly => "AND author IS NONE",
                AuthorFilter::Agent(_) => "AND author = $aa_author_id",
            };
            let budget = (primary_cap - memories.len()).min(5) as i64;
            // Two-statement approach: first get memory IDs via involves edges, then fetch memories.
            match db
                .query(
                    "SELECT record::id(in) AS mid FROM involves WHERE out INSIDE $eids LIMIT 50",
                )
                .bind(("eids", alias_rids))
                .await
            {
                Ok(mut res) => {
                    let inv_rows: Vec<serde_json::Value> = res.take(0).unwrap_or_default();
                    let alias_mem_ids: Vec<String> = inv_rows
                        .iter()
                        .filter_map(|v| {
                            v.get("mid").and_then(|s| s.as_str()).map(String::from)
                        })
                        .filter(|id| !existing_ids.contains(id))
                        .collect();

                    if !alias_mem_ids.is_empty() {
                        let aa_sql2 = format!(
                            "SELECT * OMIT vector FROM memory
                             WHERE {BITEMPORAL_ACTIVE} {visibility_clause} {proj_clause} {author_clause_aa}
                               AND record::id(id) IN $alias_mem_ids
                             ORDER BY importance DESC, event_at DESC LIMIT $aa_lim"
                        );
                        let mut q2 = db.query(&aa_sql2);
                        q2 = q2.bind(("alias_mem_ids", alias_mem_ids));
                        q2 = q2.bind(("aa_lim", budget));
                        if let Some(p) = project {
                            q2 = q2.bind(("aa_project", p.to_string()));
                        }
                        if let AuthorFilter::Agent(aid) = author_filter {
                            let rid = RecordId::new("agent", aid.as_str());
                            q2 = q2.bind(("aa_author_id", rid));
                        }
                        match q2.await {
                            Ok(mut res2) => {
                                let rows2: Vec<serde_json::Value> =
                                    res2.take(0).unwrap_or_default();
                                for mem in parse_rows(&rows2) {
                                    if !existing_ids.contains(mem.id.as_str()) {
                                        memories.push(mem);
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::debug!("alias anchor memory fetch failed: {e}");
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!("alias anchor involves query failed: {e}");
                }
            }
        }
    }

    // ── Step 5: Spreading activation (one-hop graph expansion) ──────────

    // Use a HashSet for O(1) membership checks instead of linear Vec scan.
    let primary_ids: Vec<String> = memories.iter().map(|m| m.id.as_str().to_string()).collect();
    let primary_id_set: HashSet<&str> = primary_ids.iter().map(|s| s.as_str()).collect();
    // Cap neighbors at configured cap (default 3); never exceed remaining budget.
    let neighbor_budget = spreading_cfg.cap.min(limit_usize.saturating_sub(memories.len()));
    let spreading_enabled = spreading_override.unwrap_or(true);
    if neighbor_budget > 0 && spreading_enabled {
        let _spread_started = Instant::now();
        // Pass a cap-limited view of the config so the fn respects the remaining budget.
        let effective_cfg = SpreadingConfig {
            cap: neighbor_budget,
            ..spreading_cfg.clone()
        };
        match spreading_activation(db, &primary_ids, &effective_cfg, project, author_filter).await {
            Ok(neighbors) => {
                for n in neighbors {
                    if !primary_id_set.contains(n.id.as_str()) {
                        memories.push(n);
                    }
                }
            }
            Err(e) => {
                tracing::debug!("spreading activation failed: {e}");
            }
        }
        recall_metrics::record(RecallStage::SpreadingActivation, _spread_started.elapsed());
    }

    // ── Step 6: SM-2 schedule update (fire-and-forget) ───────────────────
    // Only BM25 intent-matched rows advance spaced-repetition intervals.
    // Appearing in a result list (vector/spreading-activation) does not count
    // as an explicit read; access_count is bumped only by get_for_drill /
    // get_and_touch (explicit user-intent paths).
    spawn_sm2_access_update(db.clone(), &bm25_rows);

    Ok(memories)
}

// ── Point-in-time recall ────────────────────────────────────────────────────

/// Return what was active at a specific moment in time.
///
/// A memory is considered "active at as_of" if created before `as_of` AND
/// either still active today OR status-changed after `as_of`.
///
/// Pipeline mirrors `recall`: BM25 → optional vector → RRF fusion.
/// The point-in-time predicate is evaluated at `as_of`, NOT `time::now()`.
#[allow(clippy::too_many_arguments)]
#[instrument(skip(db, query, query_vector, reranker))]
pub async fn recall_at(
    db: &SurrealClient,
    query: &str,
    query_vector: Option<Vec<f32>>,
    reranker: Option<&Reranker>,
    as_of: DateTime<Utc>,
    project: Option<&str>,
    domain: Option<&str>,
    channel: Option<Channel>,
    modality: Option<Modality>,
    limit: u8,
    author_filter: &AuthorFilter,
    fusion_cfg: &FusionConfig,
    exclude_retracted: bool,
    mmr_cfg: &MmrConfig,
) -> Result<Vec<MemorySummary>> {
    // recall_at is a valid-time slice: returns memories whose valid-time interval covered
    // $as_of, regardless of when they were later invalidated in transaction-time (ADR-011).
    //
    // Exception — retractions (invalidation_reason = 'retracted'): a retraction means the
    // user explicitly disowned a fact as wrong, which is qualitatively different from
    // "superseded by newer data". By default (exclude_retracted=true) retracted memories are
    // hidden even from point-in-time queries so LLM callers never receive information the
    // user disowned. Pass exclude_retracted=false for forensic/audit work.
    let mut conditions = vec![
        "created_at <= $as_of".to_string(),
        "(event_at IS NONE OR event_at <= $as_of)".to_string(),
        "(valid_until IS NONE OR valid_until > $as_of)".to_string(),
        // TTL deadline: memory was not yet expired at the requested point in time
        "(expires_at IS NONE OR expires_at > $as_of)".to_string(),
    ];

    if exclude_retracted {
        // Exclude memories explicitly marked wrong by the user. Superseded, forgotten, and
        // consolidated memories are still returned — they represent valid historical state.
        conditions.push(
            "(invalidated_at IS NONE OR invalidation_reason != $retracted_reason)".to_string(),
        );
    }

    // Visibility: project-scoped excludes sensitive; cross-project excludes private + sensitive
    if project.is_some() {
        conditions.push("visibility != 'sensitive'".to_string());
    } else {
        conditions.push("visibility = 'standard'".to_string());
    }

    if project.is_some() {
        conditions.push("project = $project".to_string());
    }
    if domain.is_some() {
        conditions.push("$domain INSIDE domain".to_string());
    }
    if channel.is_some() {
        conditions.push("channel = $channel".to_string());
    }
    if modality.is_some() {
        conditions.push("modality = $modality".to_string());
    }
    match author_filter {
        AuthorFilter::None => {}
        AuthorFilter::UserOnly => conditions.push("author IS NONE".to_string()),
        AuthorFilter::Agent(_) => conditions.push("author = $at_author_id".to_string()),
    }

    let where_clause = conditions.join(" AND ");
    let pool = pool_size(limit, fusion_cfg);
    let fetch_limit = pool as i64;
    let limit_usize = limit.min(50) as usize;

    // ── BM25 full-text search ────────────────────────────────────────────

    let bm25_sql = if query.trim().is_empty() {
        format!(
            "SELECT *, 0.0 AS _bm25_score OMIT vector FROM memory
             WHERE {where_clause}
             ORDER BY importance DESC, event_at DESC
             LIMIT $limit"
        )
    } else {
        format!(
            "SELECT *, ((search::score(0) ?? 0) + (search::score(1) ?? 0) + (search::score(2) ?? 0)) AS _bm25_score OMIT vector FROM memory
             WHERE ({where_clause})
               AND (summary @0@ $query OR content @1@ $query OR anchor_entities @2@ $query)
             ORDER BY _bm25_score DESC, importance DESC
             LIMIT $limit"
        )
    };

    let mut bm25_q = db.query(&bm25_sql);
    bm25_q = bm25_q.bind(("query", query.to_string()));
    bm25_q = bm25_q.bind(("as_of", as_of));
    bm25_q = bm25_q.bind(("limit", fetch_limit));
    bm25_q = bm25_q.bind(("retracted_reason", InvalidationReason::Retracted.as_str()));
    if let Some(p) = project {
        bm25_q = bm25_q.bind(("project", p.to_string()));
    }
    if let Some(d) = domain {
        bm25_q = bm25_q.bind(("domain", d.to_string()));
    }
    bm25_q = bm25_q.bind(("channel", channel.map(|c| c.to_string())));
    bm25_q = bm25_q.bind(("modality", modality.map(|m| m.to_string())));
    if let AuthorFilter::Agent(aid) = author_filter {
        let rid = RecordId::new("agent", aid.as_str());
        bm25_q = bm25_q.bind(("at_author_id", rid));
    }

    let mut bm25_result = bm25_q
        .await
        .map_err(|e| KbError::db("recall_at BM25 query", e))?;
    let bm25_rows: Vec<serde_json::Value> = bm25_result
        .take(0)
        .map_err(|e| KbError::db("deserializing recall_at BM25 results", e))?;

    let bm25_raw_scores_at = extract_scores(&bm25_rows);
    let bm25_memories = parse_rows(&bm25_rows);
    let mut content_map_at = build_content_map(&bm25_rows);

    // ── Vector similarity search (optional) ──────────────────────────────
    // recall_at is a valid-time slice; invalidated_at (transaction-time) is intentionally
    // excluded — see ADR-011 update. Mirrors the BM25 arm predicate exactly.

    let _at_qv_present = query_vector.is_some();
    let (vector_memories, vec_raw_scores_at) = if let Some(qv) = query_vector {
        let visibility_filter = if project.is_some() {
            " AND visibility != 'sensitive'"
        } else {
            " AND visibility = 'standard'"
        };
        let author_filter_str = match author_filter {
            AuthorFilter::None => "",
            AuthorFilter::UserOnly => " AND author IS NONE",
            AuthorFilter::Agent(_) => " AND author = $at_vec_author_id",
        };
        let retracted_filter_str = if exclude_retracted {
            " AND (invalidated_at IS NONE OR invalidation_reason != $retracted_reason)"
        } else {
            ""
        };
        let knn = crate::db::vector_index::candidate_predicate(fetch_limit as usize * 4);
        let vector_sql = format!(
            "SELECT *, vector::similarity::cosine(vector, $qv) AS _vscore OMIT vector FROM memory
             WHERE created_at <= $as_of
               AND (event_at IS NONE OR event_at <= $as_of)
               AND (valid_until IS NONE OR valid_until > $as_of)
               AND (expires_at IS NONE OR expires_at > $as_of)
               AND {knn}{visibility_filter}{project_filter}{domain_filter}{channel_filter}{modality_filter}{author_filter_str}{retracted_filter_str}
             ORDER BY _vscore DESC
             LIMIT $limit",
            project_filter = if project.is_some() { " AND project = $project" } else { "" },
            domain_filter = if domain.is_some() { " AND $domain INSIDE domain" } else { "" },
            channel_filter = if channel.is_some() { " AND channel = $channel" } else { "" },
            modality_filter = if modality.is_some() { " AND modality = $modality" } else { "" },
        );

        let mut vec_q = db.query(&vector_sql);
        vec_q = vec_q.bind(("qv", qv));
        vec_q = vec_q.bind(("as_of", as_of));
        vec_q = vec_q.bind(("limit", fetch_limit));
        vec_q = vec_q.bind(("retracted_reason", InvalidationReason::Retracted.as_str()));
        if let Some(p) = project {
            vec_q = vec_q.bind(("project", p.to_string()));
        }
        if let Some(d) = domain {
            vec_q = vec_q.bind(("domain", d.to_string()));
        }
        vec_q = vec_q.bind(("channel", channel.map(|c| c.to_string())));
        vec_q = vec_q.bind(("modality", modality.map(|m| m.to_string())));
        if let AuthorFilter::Agent(aid) = author_filter {
            let rid = RecordId::new("agent", aid.as_str());
            vec_q = vec_q.bind(("at_vec_author_id", rid));
        }

        match vec_q.await {
            Ok(mut res) => {
                let rows: Vec<serde_json::Value> = match res.take(0) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("deserializing recall_at vector results: {e}");
                        vec![]
                    }
                };
                content_map_at.extend(build_content_map(&rows));
                let vscores = extract_vscores(&rows);
                (parse_rows(&rows), vscores)
            }
            Err(e) => {
                tracing::warn!("recall_at vector search failed, using BM25 only: {e}");
                (vec![], vec![])
            }
        }
    } else {
        (vec![], vec![])
    };

    // ── Fusion (RRF or Convex Combination) ────────────────────────────────

    let (mut memories, rrf_scores_at) = if vector_memories.is_empty() {
        let ranked: Vec<MemorySummary> = bm25_memories
            .into_iter()
            .enumerate()
            .filter(|(rank, _)| rrf_score(*rank) >= 0.0)
            .take(pool)
            .map(|(_, m)| m)
            .collect();
        let scores: Vec<f64> = (0..ranked.len()).map(rrf_score).collect();
        (ranked, scores)
    } else {
        match fusion_cfg.strategy {
            FusionStrategy::Rrf => rrf_fuse(&bm25_memories, &vector_memories, pool, 0.0),
            FusionStrategy::ConvexCombination => convex_fuse(
                &bm25_memories,
                &bm25_raw_scores_at,
                &vector_memories,
                &vec_raw_scores_at,
                fusion_cfg.vector_weight,
                pool,
                fusion_cfg.min_score,
            ),
        }
    };

    // ── Step 3.4: MMR diversification (ADR-031, post-fusion) ─────────
    let mut rrf_scores_at = rrf_scores_at;
    if mmr_cfg.enabled
        && !vector_memories.is_empty()
        && memories.len() > 1
        && _at_qv_present
    {
        let cand_ids: Vec<MemoryId> = memories.iter().map(|m| m.id.clone()).collect();
        let candidate_vectors = fetch_candidate_vectors(db, &cand_ids)
            .await
            .unwrap_or_default();
        let mmr_start = Instant::now();
        let pool_out = memories.len();
        let (m_new, s_new) =
            mmr_diversify(memories, rrf_scores_at, candidate_vectors, mmr_cfg.lambda, pool_out);
        memories = m_new;
        rrf_scores_at = s_new;
        recall_metrics::record(RecallStage::Mmr, mmr_start.elapsed());
    }

    // ── Step 3.5: Cross-encoder rerank (ADR-017, ADR-022) ────────────
    // recall_at uses default Adaptive policy; no caller override (deferred per ADR-022).
    if let Some(r) = reranker
        && !memories.is_empty()
        && should_rerank(query, &rrf_scores_at, None, None, None, RerankerPolicy::Adaptive)
    {
        tracing::info!(
            target: "rerank.decision",
            policy = ?RerankerPolicy::Adaptive,
            decision = true,
            "rerank gate (recall_at)"
        );
        let candidates = to_rerank_candidates(memories, &content_map_at);
        memories = r.rerank(query, candidates).await;
    } else if reranker.is_some() {
        tracing::info!(
            target: "rerank.decision",
            policy = ?RerankerPolicy::Adaptive,
            decision = false,
            "rerank gate (recall_at)"
        );
    }

    memories.truncate(limit_usize);
    Ok(memories)
}

// ── Status query ────────────────────────────────────────────────────────────

/// Query memories for status report.
///
/// Each sub-query is issued separately so that `$memory_type` and `$subtype`
/// can be bound as typed enum parameters — no raw string literals for enum
/// values appear in the SQL text.
pub async fn status_query(
    db: &SurrealClient,
    project: Option<&str>,
    domain: Option<&str>,
) -> Result<StatusReport> {
    let project_filter = match project {
        Some(_) => " AND project = $project",
        None => "",
    };
    let domain_filter = match domain {
        Some(_) => " AND $domain INSIDE domain",
        None => "",
    };
    // Cross-project status: standard only. Project-scoped: exclude sensitive.
    let visibility_filter = if project.is_some() {
        "AND visibility != 'sensitive'"
    } else {
        "AND visibility = 'standard'"
    };

    // ── action items ────────────────────────────────────────────────────────
    let action_sql = format!(
        "SELECT * OMIT vector FROM memory WHERE {BITEMPORAL_ACTIVE} {visibility_filter} \
         AND memory_type = $memory_type AND subtype = $subtype\
         {project_filter}{domain_filter} \
         ORDER BY importance DESC, event_at DESC LIMIT 10"
    );
    let mut action_q = db
        .query(&action_sql)
        .bind(("memory_type", MemoryType::Prospective.as_str()))
        .bind(("subtype", MemorySubtype::ActionItem.as_str()));
    if let Some(p) = project { action_q = action_q.bind(("project", p.to_string())); }
    if let Some(d) = domain  { action_q = action_q.bind(("domain",  d.to_string())); }
    let mut action_result = action_q.await.map_err(|e| KbError::db("status action_items query", e))?;
    let action_rows: Vec<serde_json::Value> = action_result
        .take(0)
        .map_err(|e| KbError::db("deserializing status action_items", e))?;

    // ── open questions ──────────────────────────────────────────────────────
    let questions_sql = format!(
        "SELECT * OMIT vector FROM memory WHERE {BITEMPORAL_ACTIVE} {visibility_filter} \
         AND memory_type = $memory_type AND subtype = $subtype\
         {project_filter}{domain_filter} \
         ORDER BY importance DESC LIMIT 10"
    );
    let mut questions_q = db
        .query(&questions_sql)
        .bind(("memory_type", MemoryType::Working.as_str()))
        .bind(("subtype", MemorySubtype::OpenQuestion.as_str()));
    if let Some(p) = project { questions_q = questions_q.bind(("project", p.to_string())); }
    if let Some(d) = domain  { questions_q = questions_q.bind(("domain",  d.to_string())); }
    let mut questions_result = questions_q.await.map_err(|e| KbError::db("status open_questions query", e))?;
    let question_rows: Vec<serde_json::Value> = questions_result
        .take(0)
        .map_err(|e| KbError::db("deserializing status open_questions", e))?;

    // ── recent decisions ────────────────────────────────────────────────────
    let decisions_sql = format!(
        "SELECT * OMIT vector FROM memory WHERE {BITEMPORAL_ACTIVE} {visibility_filter} \
         AND memory_type = $memory_type AND subtype = $subtype\
         {project_filter}{domain_filter} \
         ORDER BY updated_at DESC LIMIT 10"
    );
    let mut decisions_q = db
        .query(&decisions_sql)
        .bind(("memory_type", MemoryType::Semantic.as_str()))
        .bind(("subtype", MemorySubtype::Decision.as_str()));
    if let Some(p) = project { decisions_q = decisions_q.bind(("project", p.to_string())); }
    if let Some(d) = domain  { decisions_q = decisions_q.bind(("domain",  d.to_string())); }
    let mut decisions_result = decisions_q.await.map_err(|e| KbError::db("status decisions query", e))?;
    let decision_rows: Vec<serde_json::Value> = decisions_result
        .take(0)
        .map_err(|e| KbError::db("deserializing status decisions", e))?;

    // ── total count ─────────────────────────────────────────────────────────
    let total_sql = format!(
        "SELECT count() AS total FROM memory WHERE {BITEMPORAL_ACTIVE} {visibility_filter}{project_filter}{domain_filter} GROUP ALL"
    );
    let mut total_q = db.query(&total_sql);
    if let Some(p) = project { total_q = total_q.bind(("project", p.to_string())); }
    if let Some(d) = domain  { total_q = total_q.bind(("domain",  d.to_string())); }
    let mut total_result = total_q.await.map_err(|e| KbError::db("status total count query", e))?;
    let total_row: Option<serde_json::Value> = total_result
        .take(0)
        .map_err(|e| KbError::db("deserializing status total count", e))?;

    // ── active count ────────────────────────────────────────────────────────
    let active_sql = format!(
        "SELECT count() AS total FROM memory WHERE {BITEMPORAL_ACTIVE} {visibility_filter}{project_filter}{domain_filter} GROUP ALL"
    );
    let mut active_q = db.query(&active_sql);
    if let Some(p) = project { active_q = active_q.bind(("project", p.to_string())); }
    if let Some(d) = domain  { active_q = active_q.bind(("domain",  d.to_string())); }
    let mut active_result = active_q.await.map_err(|e| KbError::db("status active count query", e))?;
    let active_row: Option<serde_json::Value> = active_result
        .take(0)
        .map_err(|e| KbError::db("deserializing status active count", e))?;

    let parse_status_rows = |rows: Vec<serde_json::Value>| -> Vec<MemorySummary> {
        rows.into_iter()
            .filter_map(|row| {
                let id = MemoryId::from(extract_id_from_row(&row));
                parse_memory_row(row, &id).ok().map(MemorySummary::from)
            })
            .collect()
    };

    Ok(StatusReport {
        open_action_items: parse_status_rows(action_rows),
        open_questions: parse_status_rows(question_rows),
        recent_decisions: parse_status_rows(decision_rows),
        alerts: vec![],
        total_memories: total_row
            .and_then(|v| v.get("total").and_then(|t| t.as_u64()))
            .unwrap_or(0),
        active_memories: active_row
            .and_then(|v| v.get("total").and_then(|t| t.as_u64()))
            .unwrap_or(0),
        diagnostics: None,
    })
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Build a map from memory-id string → content (Option<String>) from raw DB rows.
///
/// Used to pair `MemorySummary` values with their content for `RerankCandidate`
/// construction after RRF fusion. The `SELECT *` queries already return `content`.
fn build_content_map(rows: &[serde_json::Value]) -> HashMap<String, Option<String>> {
    rows.iter()
        .filter_map(|row| {
            let id = extract_id_from_row(row);
            if id.is_empty() {
                return None;
            }
            let content = row
                .get("content")
                .and_then(|v| v.as_str())
                .map(String::from);
            Some((id, content))
        })
        .collect()
}

/// Convert a `Vec<MemorySummary>` into `Vec<RerankCandidate>` using a pre-built
/// content map (id → Option<content>).
fn to_rerank_candidates(
    memories: Vec<MemorySummary>,
    content_map: &HashMap<String, Option<String>>,
) -> Vec<RerankCandidate> {
    memories
        .into_iter()
        .map(|m| {
            let content = content_map
                .get(m.id.as_str())
                .and_then(|c| c.clone());
            RerankCandidate { summary: m, content }
        })
        .collect()
}

fn parse_rows(rows: &[serde_json::Value]) -> Vec<MemorySummary> {
    rows.iter()
        .filter_map(|row| {
            let id_str = extract_id_from_row(row);
            let id = MemoryId::from(id_str);
            match parse_memory_row(row.clone(), &id) {
                Ok(mem) => Some(MemorySummary::from(mem)),
                Err(e) => {
                    tracing::warn!("skipping malformed memory row: {e}");
                    None
                }
            }
        })
        .collect()
}

fn extract_id_from_row(row: &serde_json::Value) -> String {
    match row.get("id") {
        Some(serde_json::Value::String(s)) => {
            s.strip_prefix("memory:").unwrap_or(s).to_string()
        }
        Some(serde_json::Value::Object(obj)) => obj
            .get("id")
            .and_then(|v| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Object(inner) => {
                    inner.get("String").and_then(|s| s.as_str().map(String::from))
                }
                _ => None,
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MemoryId, MemorySubtype, MemoryType};

    fn make_summary(id: &str) -> MemorySummary {
        MemorySummary {
            id: MemoryId::from(id.to_string()),
            summary: id.to_string(),
            memory_type: MemoryType::Semantic,
            subtype: MemorySubtype::Fact,
            project: None,
            domain: vec![],
            importance: crate::types::Importance::default(),
            event_at: chrono::Utc::now(),
            tags: vec![],
        }
    }

    #[test]
    fn rrf_fuse_item_in_both_lists_scores_higher() {
        // "shared" appears in both bm25 (rank 0) and vector (rank 0) → highest score.
        // "bm25_only" appears only in bm25 (rank 1) → lower score.
        let bm25 = vec![make_summary("shared"), make_summary("bm25_only")];
        let vector = vec![make_summary("shared"), make_summary("vec_only")];

        let (fused, scores) = rrf_fuse(&bm25, &vector, 10, 0.0);

        assert_eq!(fused.len(), scores.len(), "scores parallel to memories");
        // "shared" must outrank both single-list items.
        let shared_pos = fused
            .iter()
            .position(|m| m.id.as_str() == "shared")
            .expect("shared must be present");
        let bm25_only_pos = fused
            .iter()
            .position(|m| m.id.as_str() == "bm25_only")
            .expect("bm25_only must be present");
        let vec_only_pos = fused
            .iter()
            .position(|m| m.id.as_str() == "vec_only")
            .expect("vec_only must be present");

        assert!(
            shared_pos < bm25_only_pos,
            "shared (both lists) should outrank bm25_only"
        );
        assert!(
            shared_pos < vec_only_pos,
            "shared (both lists) should outrank vec_only"
        );
        // Scores must be descending.
        assert!(scores[0] >= scores[1], "scores must be descending");
    }

    #[test]
    fn rrf_fuse_item_only_in_vector_list_is_included() {
        let bm25 = vec![make_summary("alpha")];
        let vector = vec![make_summary("beta_vector_only")];

        let (fused, _scores) = rrf_fuse(&bm25, &vector, 10, 0.0);

        assert!(
            fused.iter().any(|m| m.id.as_str() == "beta_vector_only"),
            "item only in vector list must survive fusion"
        );
    }

    #[test]
    fn rrf_fuse_respects_limit() {
        // 5 unique items total across both lists — limit to 3.
        let bm25: Vec<MemorySummary> = (0..5).map(|i| make_summary(&format!("bm25_{i}"))).collect();
        let vector: Vec<MemorySummary> = (0..5).map(|i| make_summary(&format!("vec_{i}"))).collect();

        let (fused, scores) = rrf_fuse(&bm25, &vector, 3, 0.0);

        assert_eq!(fused.len(), 3, "result must be truncated to limit=3");
        assert_eq!(scores.len(), 3, "scores length matches fused length");
    }

    // ── should_rerank unit tests (ADR-022) ───────────────────────────────────

    #[test]
    fn should_rerank_always_policy_returns_true() {
        assert!(should_rerank("any query", &[0.05, 0.04], None, None, None, RerankerPolicy::Always));
        assert!(should_rerank("x", &[], None, None, Some(false), RerankerPolicy::Always));
    }

    #[test]
    fn should_rerank_never_policy_returns_false() {
        assert!(!should_rerank("long query with many words here", &[0.05, 0.04], Some("alice"), Some(MemoryType::Semantic), None, RerankerPolicy::Never));
        assert!(!should_rerank("x", &[0.5], None, None, Some(true), RerankerPolicy::Never));
    }

    #[test]
    fn should_rerank_adaptive_caller_override_true() {
        // Dominant top-1 would normally skip, but caller forces rerank.
        assert!(should_rerank("hi", &[0.10, 0.04], None, None, Some(true), RerankerPolicy::Adaptive));
    }

    #[test]
    fn should_rerank_adaptive_caller_override_false() {
        // Long query + person would normally force rerank, but caller suppresses.
        assert!(!should_rerank("what did alice say about the project deadline", &[0.05, 0.04], Some("alice"), None, Some(false), RerankerPolicy::Adaptive));
    }

    #[test]
    fn should_rerank_adaptive_dominant_top1_skips() {
        // rrf[0] > 2.0 * rrf[1] → skip.
        assert!(!should_rerank("hello world", &[0.10, 0.04], None, None, None, RerankerPolicy::Adaptive));
    }

    #[test]
    fn should_rerank_adaptive_working_type_skips() {
        assert!(!should_rerank("what is my priority", &[0.05, 0.04], None, Some(MemoryType::Working), None, RerankerPolicy::Adaptive));
    }

    #[test]
    fn should_rerank_adaptive_person_filter_forces() {
        // Short query but person filter → force.
        assert!(should_rerank("alice", &[0.05, 0.04], Some("alice"), None, None, RerankerPolicy::Adaptive));
    }

    #[test]
    fn should_rerank_adaptive_long_query_forces() {
        // >6 words → force.
        assert!(should_rerank("what did I decide about the database schema last week", &[0.05, 0.04], None, None, None, RerankerPolicy::Adaptive));
    }

    #[test]
    fn should_rerank_adaptive_short_query_no_person_skips() {
        // ≤3 words, no person → skip.
        assert!(!should_rerank("database schema", &[0.05, 0.04], None, None, None, RerankerPolicy::Adaptive));
    }

    #[test]
    fn should_rerank_adaptive_medium_query_semantic_forces() {
        // 4-6 words + Semantic → true.
        assert!(should_rerank("what is my architecture decision", &[0.05, 0.04], None, Some(MemoryType::Semantic), None, RerankerPolicy::Adaptive));
    }

    #[test]
    fn should_rerank_adaptive_medium_query_episodic_reranks() {
        // 4-6 words + Episodic, no person → true (no Semantic-only fallthrough).
        assert!(should_rerank("what happened at the meeting", &[0.05, 0.04], None, Some(MemoryType::Episodic), None, RerankerPolicy::Adaptive));
    }

    // ── min_max_normalize tests ──────────────────────────────────────────────

    #[test]
    fn min_max_normalize_empty_returns_empty() {
        let result = min_max_normalize(std::iter::empty());
        assert!(result.is_empty());
    }

    #[test]
    fn min_max_normalize_constant_all_ones() {
        // All equal scores → range == 0 → all mapped to 1.0 (no div-by-zero).
        let result = min_max_normalize([5.0, 5.0, 5.0].into_iter());
        assert_eq!(result.len(), 3);
        assert!(result.iter().all(|&v| (v - 1.0).abs() < f64::EPSILON));
    }

    #[test]
    fn min_max_normalize_range() {
        let result = min_max_normalize([0.0, 5.0, 10.0].into_iter());
        assert_eq!(result.len(), 3);
        assert!((result[0] - 0.0).abs() < f64::EPSILON);
        assert!((result[1] - 0.5).abs() < f64::EPSILON);
        assert!((result[2] - 1.0).abs() < f64::EPSILON);
    }

    // ── convex_fuse tests ────────────────────────────────────────────────────

    #[test]
    fn convex_fuse_single_source_bm25_only() {
        // Only BM25 results, vector empty. vector_weight=0.0 so only BM25 contributes.
        // Items normalise to [0,1] by min-max; output order preserves BM25 rank.
        let bm25 = vec![make_summary("a"), make_summary("b"), make_summary("c")];
        let bm25_scores = vec![3.0, 2.0, 1.0];
        let (fused, scores) = convex_fuse(&bm25, &bm25_scores, &[], &[], 0.0, 10, 0.0);
        // All weight goes to BM25; best BM25 score gets CC=1.0, worst gets CC=0.0.
        assert_eq!(fused.len(), 3);
        assert_eq!(fused[0].id.as_str(), "a", "highest BM25 score should rank first");
        assert!(scores[0] > scores[1], "scores must be descending");
        assert!(scores[1] > scores[2], "scores must be descending");
    }

    #[test]
    fn convex_fuse_balanced_weights() {
        // Both lists with alpha=0.5. Verify final = 0.5 * norm_v + 0.5 * norm_bm25.
        // Item "x": bm25=10 (max→1.0 norm), vec=1.0 (max→1.0 norm) → CC = 0.5+0.5 = 1.0
        // Item "y": bm25=0  (min→0.0 norm), vec=0.0 (min→0.0 norm) → CC = 0.0
        let bm25 = vec![make_summary("x"), make_summary("y")];
        let bm25_scores = vec![10.0, 0.0];
        let vector = vec![make_summary("x"), make_summary("y")];
        let vec_scores = vec![1.0, 0.0];
        let (fused, scores) = convex_fuse(&bm25, &bm25_scores, &vector, &vec_scores, 0.5, 10, 0.0);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].id.as_str(), "x");
        assert!((scores[0] - 1.0).abs() < 1e-9);
        assert!((scores[1] - 0.0).abs() < 1e-9);
    }

    #[test]
    fn convex_fuse_vector_dominant() {
        // vector_weight=0.9: item top of vector wins even if it has poor BM25 rank.
        // "vec_top": bm25_score=0 (rank last), vec_score=1.0 (rank first)
        // "bm25_top": bm25_score=1.0 (rank first), vec_score=0.0 (rank last)
        // CC("vec_top") = 0.9*1.0 + 0.1*0.0 = 0.9
        // CC("bm25_top") = 0.9*0.0 + 0.1*1.0 = 0.1
        let bm25 = vec![make_summary("bm25_top"), make_summary("vec_top")];
        let bm25_scores = vec![1.0, 0.0];
        let vector = vec![make_summary("vec_top"), make_summary("bm25_top")];
        let vec_scores = vec![1.0, 0.0];
        let (fused, scores) = convex_fuse(&bm25, &bm25_scores, &vector, &vec_scores, 0.9, 10, 0.0);
        assert_eq!(fused[0].id.as_str(), "vec_top", "vector-dominant: vec_top must win");
        assert!(scores[0] > scores[1]);
    }

    #[test]
    fn convex_fuse_min_score_filter() {
        // min_score=0.5 should drop items with CC < 0.5.
        // Item "high": CC=1.0 (kept), "low": CC=0.0 (dropped).
        let bm25 = vec![make_summary("high"), make_summary("low")];
        let bm25_scores = vec![1.0, 0.0];
        let vector = vec![make_summary("high"), make_summary("low")];
        let vec_scores = vec![1.0, 0.0];
        let (fused, _scores) = convex_fuse(&bm25, &bm25_scores, &vector, &vec_scores, 0.5, 10, 0.5);
        assert_eq!(fused.len(), 1, "only 'high' should survive min_score=0.5");
        assert_eq!(fused[0].id.as_str(), "high");
    }
}
