use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use tracing::instrument;

use crate::db::client::SurrealClient;
use crate::db::helpers::enum_to_value;
use crate::db::memory::parse_memory_row;
use crate::error::{KbError, Result};
use crate::types::*;

// ── Reciprocal Rank Fusion ──────────────────────────────────────────────────

/// RRF constant k — controls how much rank matters vs. flat contribution.
/// k=60 is the standard from Cormack et al. (2009).
const RRF_K: f64 = 60.0;

fn rrf_score(rank: usize) -> f64 {
    1.0 / (RRF_K + rank as f64 + 1.0)
}

/// Fuse two ranked result lists into a single list via Reciprocal Rank Fusion.
///
/// Each source contributes `1/(k + rank + 1)` per item. Items appearing in both
/// sources get summed scores. Returns results sorted by descending fused score,
/// truncated to `limit`.
fn rrf_fuse(
    bm25: &[MemorySummary],
    vector: &[MemorySummary],
    limit: usize,
) -> Vec<MemorySummary> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut by_id: HashMap<String, MemorySummary> = HashMap::new();

    for (rank, mem) in bm25.iter().enumerate() {
        let id = mem.id.as_str().to_string();
        *scores.entry(id.clone()).or_default() += rrf_score(rank);
        by_id.entry(id).or_insert_with(|| mem.clone());
    }
    for (rank, mem) in vector.iter().enumerate() {
        let id = mem.id.as_str().to_string();
        *scores.entry(id.clone()).or_default() += rrf_score(rank);
        by_id.entry(id).or_insert_with(|| mem.clone());
    }

    let mut fused: Vec<(f64, MemorySummary)> = scores
        .into_iter()
        .filter_map(|(id, score)| by_id.remove(&id).map(|m| (score, m)))
        .collect();

    fused.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    fused.truncate(limit);
    fused.into_iter().map(|(_, m)| m).collect()
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
        // Batch all SM-2 updates into a single chained query to avoid N+1.
        // Values are server-computed (not user input), so format interpolation is safe.
        let parts: Vec<String> = updates
            .iter()
            .map(|u| {
                format!(
                    "UPDATE type::record('memory', '{}') SET ease_factor = {}, review_interval = {}, next_review_at = '{}'",
                    u.id, u.ease_factor, u.review_interval, u.next_review_at.to_rfc3339()
                )
            })
            .collect();
        let batch_sql = parts.join("; ");
        if let Err(e) = db.query(&batch_sql).await {
            tracing::warn!("SM-2 batch update failed: {e}");
        }
    });
}

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
    // Cap at 365 days — larger intervals are meaningless for review scheduling.
    let new_ri = ((ri as f64) * ef).ceil().clamp(1.0, 365.0) as i64;
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
/// relates, AND involves edges. Returns active memories not already in `known_ids`.
///
/// The involves traversal walks: memory→involves→entity→involves←memory,
/// discovering "other memories about the same person/entity."
async fn spreading_activation(
    db: &SurrealClient,
    known_ids: &[String],
    limit: usize,
) -> Result<Vec<MemorySummary>> {
    if known_ids.is_empty() || limit == 0 {
        return Ok(vec![]);
    }

    // Collect neighbor IDs from graph edges (both directions).
    // Each edge type produces an array of record IDs per source memory.
    // Includes involves: memory→involves→entity→involves←memory
    let mut result = db
        .query(
            "SELECT
                array::flatten(->extends->memory.id) AS ext_out,
                array::flatten(<-extends<-memory.id) AS ext_in,
                array::flatten(->derives->memory.id) AS der_out,
                array::flatten(<-derives<-memory.id) AS der_in,
                array::flatten(->relates->memory.id) AS rel_out,
                array::flatten(<-relates<-memory.id) AS rel_in,
                array::flatten(->involves->entity<-involves<-memory.id) AS inv_via_entity
             FROM memory WHERE record::id(id) IN $ids",
        )
        .bind(("ids", known_ids.to_vec()))
        .await
        .map_err(|e| KbError::db("spreading activation neighbor query", e))?;

    let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();

    // Flatten all neighbor IDs into a deduplicated set, excluding known IDs.
    let known_set: std::collections::HashSet<&str> =
        known_ids.iter().map(|s| s.as_str()).collect();
    let mut neighbor_ids: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

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

    for row in &rows {
        for field in &[
            "ext_out", "ext_in", "der_out", "der_in", "rel_out", "rel_in",
            "inv_via_entity",
        ] {
            for id in extract_ids_from_field(row, field) {
                if !known_set.contains(id.as_str()) && seen.insert(id.clone()) {
                    neighbor_ids.push(id);
                }
            }
        }
    }

    if neighbor_ids.is_empty() {
        return Ok(vec![]);
    }

    // Fetch neighbor memory records (active only, standard visibility only).
    // Spreading activation is automatic cross-context expansion — never
    // surface private or sensitive memories through graph traversal.
    let neighbor_limit = limit as i64;
    let mut result = db
        .query(
            "SELECT * FROM memory
             WHERE status = 'active'
               AND visibility = 'standard'
               AND record::id(id) IN $ids
             ORDER BY importance DESC
             LIMIT $limit",
        )
        .bind(("ids", neighbor_ids))
        .bind(("limit", neighbor_limit))
        .await
        .map_err(|e| KbError::db("spreading activation fetch neighbors", e))?;

    let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();

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
#[instrument(skip(db, query, query_vector))]
pub async fn recall(
    db: &SurrealClient,
    query: &str,
    query_vector: Option<Vec<f32>>,
    memory_type: Option<MemoryType>,
    project: Option<&str>,
    domain: Option<&str>,
    person: Option<&str>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    limit: u8,
) -> Result<Vec<MemorySummary>> {
    let fetch_limit = limit.min(25) as i64;

    // For entity-anchored queries, increase BM25 fetch to ensure we have
    // enough candidates before the entity intersection in Step 4.
    let bm25_fetch = if is_entity_anchored(person) {
        (fetch_limit * 2).min(50)
    } else {
        fetch_limit
    };

    // ── Step 1: BM25 full-text search ───────────────────────────────────

    let mut conditions = vec!["status = 'active'".to_string()];

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
    let where_clause = conditions.join(" AND ");

    let bm25_sql = if query.trim().is_empty() {
        format!(
            "SELECT * FROM memory
             WHERE {where_clause}
             ORDER BY importance DESC, event_at DESC
             LIMIT $limit"
        )
    } else {
        format!(
            "SELECT * FROM memory
             WHERE ({where_clause})
               AND (summary @@ $query OR content @@ $query)
             ORDER BY importance DESC, event_at DESC
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

    let mut bm25_result = bm25_q
        .await
        .map_err(|e| KbError::db("BM25 recall query", e))?;
    let bm25_rows: Vec<serde_json::Value> = bm25_result
        .take(0)
        .map_err(|e| KbError::db("deserializing BM25 recall results", e))?;

    let bm25_memories = parse_rows(&bm25_rows);

    // ── Step 2: Vector similarity search (optional) ─────────────────────

    let vector_memories = if let Some(ref qv) = query_vector {
        // Brute-force cosine similarity scan. SurrealKV embedded doesn't support
        // MTREE ANN indexes yet, but brute-force is fast enough for a personal KB
        // with <100k records.
        let visibility_filter = if project.is_some() {
            " AND visibility != 'sensitive'"
        } else {
            " AND visibility = 'standard'"
        };
        let vector_sql = format!(
            "SELECT *, vector::similarity::cosine(vector, $qv) AS _vscore FROM memory
             WHERE status = 'active' AND vector != NONE{visibility_filter}{type_filter}{project_filter}{domain_filter}{since_filter}{until_filter}
             ORDER BY _vscore DESC
             LIMIT $limit",
            type_filter = if memory_type.is_some() { " AND memory_type = $memory_type" } else { "" },
            project_filter = if project.is_some() { " AND project = $project" } else { "" },
            domain_filter = if domain.is_some() { " AND $domain INSIDE domain" } else { "" },
            since_filter = if since.is_some() { " AND event_at >= $since" } else { "" },
            until_filter = if until.is_some() { " AND event_at <= $until" } else { "" },
        );

        let mut vec_q = db.query(&vector_sql);
        vec_q = vec_q.bind(("qv", qv.clone()));
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

        match vec_q.await {
            Ok(mut res) => {
                let rows: Vec<serde_json::Value> = res.take(0).unwrap_or_default();
                parse_rows(&rows)
            }
            Err(e) => {
                // Vector search is best-effort — fall back to BM25 only.
                tracing::warn!("vector search failed, using BM25 only: {e}");
                vec![]
            }
        }
    } else {
        vec![]
    };

    // ── Step 3: RRF fusion (always balanced) ──────────────────────────
    // DA review: intent-based boosting was mathematically imprecise (duplicating
    // list items). Always use balanced RRF — it's robust to one source being weak.

    let limit_usize = limit.min(25) as usize;
    let mut memories = if vector_memories.is_empty() {
        bm25_memories
    } else {
        rrf_fuse(&bm25_memories, &vector_memories, limit_usize)
    };

    // ── Step 4: Entity-anchored person filter ─────────────────────────
    //
    // When a person filter is present, we start from the entity's `involves`
    // edges to collect ALL memory IDs for that person, then intersect with
    // text-ranked results. This avoids the old post-hoc bug where valid
    // results ranked beyond the BM25 limit were dropped.

    if let Some(person_name) = person {
        // Collect all memory IDs linked to this entity
        let mut entity_result = db
            .query(
                "SELECT array::flatten(<-involves<-memory.id) AS mem_ids FROM entity
                 WHERE string::lowercase(name) CONTAINS string::lowercase($name)",
            )
            .bind(("name", person_name.to_string()))
            .await
            .map_err(|e| KbError::db("entity-anchored query", e))?;

        let mut person_result = db
            .query(
                "SELECT array::flatten(<-involves<-memory.id) AS mem_ids FROM person
                 WHERE string::lowercase(name) CONTAINS string::lowercase($name)",
            )
            .bind(("name", person_name.to_string()))
            .await
            .map_err(|e| KbError::db("person-anchored query", e))?;

        let mut entity_mem_ids = std::collections::HashSet::new();

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

        let entity_rows: Vec<serde_json::Value> = entity_result
            .take(0)
            .map_err(|e| KbError::db("deserializing entity filter", e))?;
        let person_rows: Vec<serde_json::Value> = person_result
            .take(0)
            .map_err(|e| KbError::db("deserializing person filter", e))?;

        for row in &entity_rows {
            for id in extract_ids_from_row(row) {
                entity_mem_ids.insert(id);
            }
        }
        for row in &person_rows {
            for id in extract_ids_from_row(row) {
                entity_mem_ids.insert(id);
            }
        }

        if entity_mem_ids.is_empty() {
            memories.clear();
        } else {
            // Keep any text-ranked results that are in the entity set
            let text_ranked: Vec<MemorySummary> = memories
                .iter()
                .filter(|m| entity_mem_ids.contains(m.id.as_str()))
                .cloned()
                .collect();

            // Also fetch entity-linked memories NOT in text results (the fix:
            // these would have been dropped by post-hoc filtering before)
            let text_ids: std::collections::HashSet<String> =
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
                let sql = format!(
                    "SELECT * FROM memory
                     WHERE status = 'active' {visibility_filter}
                       AND record::id(id) IN $ids
                     ORDER BY importance DESC, event_at DESC
                     LIMIT $limit"
                );
                let extra_limit = (limit_usize.saturating_sub(text_ranked.len())) as i64;
                if extra_limit > 0 {
                    let mut q = db.query(&sql);
                    q = q.bind(("ids", missing_ids));
                    q = q.bind(("limit", extra_limit));
                    if let Ok(mut res) = q.await {
                        let rows: Vec<serde_json::Value> = res.take(0).unwrap_or_default();
                        extra_memories = parse_rows(&rows);
                    }
                }
            }

            memories = text_ranked;
            memories.extend(extra_memories);
            memories.truncate(limit_usize);
        }
    }

    // ── Step 5: Spreading activation (one-hop graph expansion) ──────────

    let primary_ids: Vec<String> = memories.iter().map(|m| m.id.as_str().to_string()).collect();
    // Cap spreading activation at 3 results (DA review: more is noise, not signal).
    let neighbor_budget = 3usize.min(limit_usize.saturating_sub(memories.len()));
    if neighbor_budget > 0 {
        match spreading_activation(db, &primary_ids, neighbor_budget).await {
            Ok(neighbors) => {
                for n in neighbors {
                    if !primary_ids.contains(&n.id.as_str().to_string()) {
                        memories.push(n);
                    }
                }
            }
            Err(e) => {
                tracing::debug!("spreading activation failed: {e}");
            }
        }
    }

    // ── Step 6: Access update (fire-and-forget) ──────────────────────────

    // Update access_count + last_accessed_at for ALL returned memories,
    // not just BM25 rows (fixes bias where vector-only results never got updates).
    let all_returned_ids: Vec<String> = memories.iter().map(|m| m.id.as_str().to_string()).collect();
    if !all_returned_ids.is_empty() {
        let db_clone = db.clone();
        tokio::spawn(async move {
            if let Err(e) = db_clone
                .query(
                    "UPDATE memory SET access_count += 1, updated_at = time::now()
                     WHERE record::id(id) IN $ids",
                )
                .bind(("ids", all_returned_ids))
                .await
            {
                tracing::warn!("access count update failed: {e}");
            }
        });
    }

    // SM-2 schedule update for BM25 rows (maintains spaced repetition intervals)
    spawn_sm2_access_update(db.clone(), &bm25_rows);

    Ok(memories)
}

// ── Point-in-time recall ────────────────────────────────────────────────────

/// Return what was active at a specific moment in time.
///
/// A memory is considered "active at as_of" if created before `as_of` AND
/// either still active today OR status-changed after `as_of`.
#[allow(clippy::too_many_arguments)]
#[instrument(skip(db, query))]
pub async fn recall_at(
    db: &SurrealClient,
    query: &str,
    as_of: DateTime<Utc>,
    project: Option<&str>,
    domain: Option<&str>,
    limit: u8,
) -> Result<Vec<MemorySummary>> {
    let mut conditions = vec![
        "created_at <= $as_of".to_string(),
        "(status = 'active' OR updated_at > $as_of)".to_string(),
    ];

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

    let where_clause = conditions.join(" AND ");
    let limit_val = limit.min(25) as i64;

    let sql = if query.trim().is_empty() {
        format!(
            "SELECT * FROM memory
             WHERE {where_clause}
             ORDER BY importance DESC, event_at DESC
             LIMIT $limit"
        )
    } else {
        format!(
            "SELECT * FROM memory
             WHERE ({where_clause})
               AND (summary @@ $query OR content @@ $query)
             ORDER BY importance DESC, event_at DESC
             LIMIT $limit"
        )
    };

    let mut q = db.query(&sql);
    q = q.bind(("query", query.to_string()));
    q = q.bind(("as_of", as_of));
    q = q.bind(("limit", limit_val));
    if let Some(p) = project {
        q = q.bind(("project", p.to_string()));
    }
    if let Some(d) = domain {
        q = q.bind(("domain", d.to_string()));
    }

    let mut result = q.await.map_err(|e| KbError::db("recall_at query", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing recall_at results", e))?;

    let memories = parse_rows(&rows);
    Ok(memories)
}

// ── Status query ────────────────────────────────────────────────────────────

/// Query memories for status report.
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

    let action_items_sql = format!(
        "SELECT * FROM memory WHERE status = 'active' {visibility_filter} AND memory_type = 'prospective' AND subtype = 'action_item'{}{} ORDER BY importance DESC, event_at DESC LIMIT 10",
        project_filter, domain_filter
    );
    let questions_sql = format!(
        "SELECT * FROM memory WHERE status = 'active' {visibility_filter} AND memory_type = 'working' AND subtype = 'open_question'{}{} ORDER BY importance DESC LIMIT 10",
        project_filter, domain_filter
    );
    let decisions_sql = format!(
        "SELECT * FROM memory WHERE status = 'active' {visibility_filter} AND memory_type = 'semantic' AND subtype = 'decision'{}{} ORDER BY updated_at DESC LIMIT 10",
        project_filter, domain_filter
    );
    let total_sql = format!(
        "SELECT count() AS total FROM memory WHERE true{}{} GROUP ALL",
        project_filter, domain_filter
    );
    let active_sql = format!(
        "SELECT count() AS total FROM memory WHERE status = 'active'{}{} GROUP ALL",
        project_filter, domain_filter
    );

    let full_query =
        format!("{action_items_sql}; {questions_sql}; {decisions_sql}; {total_sql}; {active_sql}");

    let mut q = db.query(&full_query);
    if let Some(p) = project {
        q = q.bind(("project", p.to_string()));
    }
    if let Some(d) = domain {
        q = q.bind(("domain", d.to_string()));
    }

    let mut result = q.await.map_err(|e| KbError::db("status query", e))?;

    let action_rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();
    let question_rows: Vec<serde_json::Value> = result.take(1).unwrap_or_default();
    let decision_rows: Vec<serde_json::Value> = result.take(2).unwrap_or_default();
    let total_row: Option<serde_json::Value> = result.take(3).unwrap_or_default();
    let active_row: Option<serde_json::Value> = result.take(4).unwrap_or_default();

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
    })
}

// ── Helpers ─────────────────────────────────────────────────────────────────

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
