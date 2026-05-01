//! Automated consolidation candidate detection and atomic consolidation write.
//!
//! `find_candidates` surfaces clusters for LLM review; it does NOT auto-merge.
//! `consolidate_atomic` performs the full CREATE + archive + RELATE sequence
//! inside a single `BEGIN TRANSACTION … COMMIT TRANSACTION` block so that no
//! partial state is ever visible.

use chrono::Utc;
use serde::Serialize;
use surrealdb::types::RecordId;
use tracing::instrument;

use crate::db::client::SurrealClient;
use crate::db::helpers::enum_to_value;
use crate::db::BITEMPORAL_ACTIVE;
use crate::error::{KbError, Result};
use crate::types::*;

/// A group of memories that are candidates for consolidation.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ConsolidationCandidate {
    /// Memory IDs in this cluster.
    pub ids: Vec<String>,
    /// Summaries of the memories (for LLM context).
    pub summaries: Vec<String>,
    /// Why these were grouped together.
    pub reason: String,
    /// Shared project (if any).
    pub project: Option<String>,
    /// Author agent ID shared by all members of the cluster (`None` = user-written).
    ///
    /// Clusters are always same-author — this field lets callers verify the bucket
    /// and drives the author field on any consolidated memory created from the cluster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<AgentId>,
}

impl ConsolidationCandidate {
    /// Convert the raw string IDs to typed `MemoryId` newtypes.
    ///
    /// `MemoryId::from` is infallible, but returning `Result` keeps the call site
    /// consistent with other fallible DB-derived conversions and lets us surface
    /// any future validation (e.g. prefix check) without breaking callers.
    pub fn memory_ids(&self) -> crate::error::Result<Vec<MemoryId>> {
        Ok(self.ids.iter().cloned().map(MemoryId::from).collect())
    }
}

/// Find clusters of memories that are candidates for consolidation.
///
/// Heuristics:
/// 1. Same project + same subtype with 3+ active memories → likely related
/// 2. Connected graph clusters (3+ memories linked via `relates` edges)
///
/// # Parameters
/// - `limit` — max candidates to return.
/// - `min_age_days` — when `Some(n)`, only include memories whose `created_at`
///   is at least `n` days in the past. `None` = no age filter (back-compat for
///   `kb_consolidation_candidates` MCP tool).
/// - `max_cluster_size` — when `Some(n)`, skip clusters with more than `n`
///   members. `None` = no cap. Filtered in Rust (consistent with cnt<3 gate).
#[instrument(skip(db))]
pub async fn find_candidates(
    db: &SurrealClient,
    limit: usize,
    min_age_days: Option<u32>,
    max_cluster_size: Option<u32>,
) -> Result<Vec<ConsolidationCandidate>> {
    let mut candidates = Vec::new();

    // Build optional age filter for heuristic 1 SQL.
    // `$min_age` is bound as a SurrealDB duration string when Some.
    let age_filter_h1 = if min_age_days.is_some() {
        "AND created_at < time::now() - $min_age".to_string()
    } else {
        String::new()
    };

    // Heuristic 1: Same-project, same-subtype, same-author clusters with 3+ members.
    //
    // `array::group` returns empty silently on SurrealKV (docs/surrealdb-v3.md §42-45),
    // so we fetch individual rows and aggregate on the Rust side.
    // Standard-only: find_candidates has no project scope, so cross-project
    // default applies — private memory summaries must not appear in results.
    // Grouping by author prevents cross-user clusters on a shared daemon.
    let h1_sql = format!(
        "SELECT record::id(id) AS mid, summary, project, subtype, author \
         FROM memory \
         WHERE {BITEMPORAL_ACTIVE} \
           AND visibility = 'standard' \
           AND project IS NOT NONE \
           {age_filter_h1} \
         LIMIT $h1_cap"
    );

    let mut h1_query = db.query(&h1_sql).bind(("h1_cap", 200_u32));
    if let Some(days) = min_age_days {
        // SurrealDB duration: "7d" for 7 days.
        h1_query = h1_query.bind(("min_age", format!("{days}d")));
    }

    let mut result = h1_query
        .await
        .map_err(|e| KbError::db("consolidation: project+subtype clusters", e))?;

    let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();

    // Aggregate into (project, subtype, author) → (ids, summaries) buckets.
    // Key: (project_string, subtype_string, author_string_or_empty)
    type H1Bucket = (Vec<String>, Vec<String>, Option<AgentId>);
    let mut h1_buckets: std::collections::HashMap<(String, String, String), H1Bucket> =
        std::collections::HashMap::new();

    for row in &rows {
        let mid = match row.get("mid").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let summary = row
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let project = match row.get("project").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let subtype = row
            .get("subtype")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let author: Option<AgentId> = row
            .get("author")
            .filter(|v| !v.is_null())
            .and_then(|v| crate::db::helpers::record_id_bare(v, "agent"))
            .filter(|s| !s.is_empty())
            .map(AgentId::from);
        let author_key = author.as_ref().map(|a| a.as_str().to_string()).unwrap_or_default();

        let entry = h1_buckets
            .entry((project, subtype, author_key))
            .or_insert_with(|| (Vec::new(), Vec::new(), author));
        entry.0.push(mid);
        entry.1.push(summary);
    }

    for ((project, subtype, _author_key), (ids, summaries, author)) in h1_buckets {
        let cnt = ids.len() as u64;
        if cnt < 3 {
            continue;
        }
        // max_cluster_size filter: skip clusters that are too large.
        if let Some(max) = max_cluster_size
            && cnt > u64::from(max)
        {
            continue;
        }

        candidates.push(ConsolidationCandidate {
            ids,
            summaries,
            reason: format!(
                "Same project + subtype ({subtype}) with 3+ memories"
            ),
            project: Some(project),
            author,
        });
    }

    // Heuristic 2: Graph clusters — standard memories with 2+ outbound `relates`
    // edges to other standard memories of the same author.
    //
    // The traversal `->relates->(memory WHERE visibility = 'standard').id` keeps
    // private neighbor IDs out of the result. Same-author restriction is applied
    // in Rust to avoid awkward SurrealDB inline record-relative filter syntax.
    //
    // We do NOT use `array::group()` here — without a matching `GROUP BY` that
    // function collapses all rows into a single aggregate in SurrealDB v3.
    //
    // min_age_days filter applied in Rust post-fetch (graph traversal context
    // makes inline SQL filtering harder; consistent with other Rust-side filters).
    let h2_sql = format!(
        "SELECT record::id(id) AS mid, summary, author, created_at,
                ->relates->(memory WHERE visibility = 'standard' AND {BITEMPORAL_ACTIVE}).id AS related_rids
         FROM memory
         WHERE {BITEMPORAL_ACTIVE}
           AND visibility = 'standard'
         LIMIT 200"
    );

    let mut result = db
        .query(&h2_sql)
        .await
        .map_err(|e| KbError::db("consolidation: graph clusters", e))?;

    let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();

    // Compute the age cutoff for heuristic 2 post-filter.
    let age_cutoff: Option<chrono::DateTime<Utc>> = min_age_days.map(|days| {
        Utc::now() - chrono::Duration::days(i64::from(days))
    });

    // Build an id → (author, summary) lookup from the batch for same-author
    // neighbor resolution without a second DB round-trip.
    let node_info: std::collections::HashMap<&str, (Option<AgentId>, &str)> = rows
        .iter()
        .filter_map(|r| {
            let mid = r.get("mid").and_then(|v| v.as_str())?;
            let summary = r.get("summary").and_then(|v| v.as_str()).unwrap_or_default();
            let author: Option<AgentId> = r
                .get("author")
                .filter(|v| !v.is_null())
                .and_then(|v| crate::db::helpers::record_id_bare(v, "agent"))
                .filter(|s| !s.is_empty())
                .map(AgentId::from);
            Some((mid, (author, summary)))
        })
        .collect();

    for row in &rows {
        let mid = row.get("mid").and_then(|v| v.as_str()).unwrap_or_default();
        let summary = row
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        // Apply min_age_days filter: skip seed nodes created too recently.
        if let Some(cutoff) = age_cutoff {
            let created_at = row
                .get("created_at")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<chrono::DateTime<Utc>>().ok());
            match created_at {
                Some(ts) if ts > cutoff => continue,
                None => continue, // missing created_at → exclude
                _ => {}
            }
        }

        // Extract the author of the seed node.
        let seed_author: Option<AgentId> = row
            .get("author")
            .filter(|v| !v.is_null())
            .and_then(|v| crate::db::helpers::record_id_bare(v, "agent"))
            .filter(|s| !s.is_empty())
            .map(AgentId::from);

        // `related_rids` is an array of record-id objects from the traversal.
        // Keep only same-author neighbors: a Standard memory must not pull
        // a different user's Standard memory into the same cluster.
        let related_rids: Vec<String> = row
            .get("related_rids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| crate::db::helpers::record_id_bare(v, "memory"))
                    .filter(|s| !s.is_empty())
                    .filter(|rid| {
                        // If the neighbor appears in our batch, check its author.
                        // If it's outside the 200-row window we can't verify —
                        // exclude conservatively rather than risk a cross-author merge.
                        node_info
                            .get(rid.as_str())
                            .is_some_and(|(nb_author, _)| nb_author == &seed_author)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // max_cluster_size: hub + neighbors must not exceed the cap.
        let cluster_size = 1 + related_rids.len();
        if let Some(max) = max_cluster_size
            && cluster_size > usize::try_from(max).unwrap_or(usize::MAX)
        {
            continue;
        }

        if related_rids.len() >= 2 && !mid.is_empty() {
            let mut ids = vec![mid.to_string()];
            let mut summaries = vec![summary.to_string()];

            for rid in &related_rids {
                ids.push(rid.clone());
                // Summary from the batch; absent if outside the 200-row window.
                if let Some((_, nb_summary)) = node_info.get(rid.as_str()) {
                    summaries.push(nb_summary.to_string());
                }
            }

            candidates.push(ConsolidationCandidate {
                ids,
                summaries,
                reason: "Graph cluster with 3+ related memories".to_string(),
                project: None,
                author: seed_author,
            });
        }
    }

    candidates.truncate(limit);
    Ok(candidates)
}

// ---------------------------------------------------------------------------
// Atomic consolidation write
// ---------------------------------------------------------------------------

/// Caller-supplied fields for the consolidated memory.
///
/// Aggregated metadata (importance, people, tags, domain, visibility, subtype,
/// project, channel, modality) is derived from source records inside
/// `consolidate_atomic` via `fetch_source_metadata`.
pub struct ConsolidateParams {
    /// Summary text for the new consolidated memory (caller-supplied, validated before call).
    pub summary: String,
    /// Pre-computed embedding vector (may be None if embedder is unavailable).
    pub vector: Option<Vec<f32>>,
    /// Forwarded agent name from the first source; drives the `source_agent` field.
    pub source_agent: Option<String>,
    /// When `Some`, overrides the inherited `min(source.event_at)`. Usually `None`.
    pub event_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Aggregated metadata derived from all source memories, computed by `fetch_source_metadata`.
struct SourceMetadata {
    memory_type: MemoryType,
    subtype: MemorySubtype,
    channel: Option<Channel>,
    modality: Option<Modality>,
    project: Option<String>,
    domain: Vec<String>,
    people: Vec<String>,
    importance: Importance,
    visibility: Visibility,
    tags: Vec<String>,
}

/// Perform the full consolidation atomically:
///   1. Query source memories to compute inherited `event_at` and `valid_until`.
///   2. CREATE the new consolidated memory (with pre-computed vector if provided).
///   3. UPDATE each source memory to status='archived' with bitemporal fields.
///   4. RELATE new_id -> derives -> each source_id.
///
/// All steps 2-4 are wrapped in a single `BEGIN TRANSACTION … COMMIT TRANSACTION`
/// so the DB is never left in a partially-consolidated state.
///
/// Per ADR-011 §"Consolidation Provenance":
/// - `event_at = min(source event_ats)` — earliest fact in the cluster.
/// - `valid_until = max(source valid_untils)` if ALL sources have `valid_until` set;
///   otherwise NULL (unknown termination).
///
/// The new `MemoryId` is generated in Rust before the transaction so we can
/// use it directly in the RELATE clauses without parsing a LET return value.
///
/// # Statement index layout (for `response.take(N)`)
///
/// SurrealDB v3 maps each semicolon-delimited statement to a sequential index,
/// including the `BEGIN` and `COMMIT` control words:
///   0  = BEGIN TRANSACTION       (empty result)
///   1  = CREATE …                (empty — we don't RETURN from it)
///   2  = UPDATE src_0            (empty)
///   …
///   2+N-1 = UPDATE src_{N-1}
///   2+N   = RELATE src_0
///   …
///   2+2N-1 = RELATE src_{N-1}
///   2+2N  = COMMIT TRANSACTION   (empty)
///
/// We consume all indices to surface any embedded SurrealQL errors.
///
/// # RELATE subjects
///
/// SurrealDB v3 requires concrete `RecordId` bindings for RELATE in/out —
/// `type::record(...)` is not accepted as a subject. Source IDs are bound as
/// `$src_rid_0 … $src_rid_{N-1}` and the new memory as `$new_rid`.
/// UPDATE statements still use `type::record('memory', $src_str_N)` (string
/// binding) because that form works for UPDATE even in v3.
#[instrument(skip(db, params), fields(source_count = source_ids.len()))]
pub async fn consolidate_atomic(
    db: &SurrealClient,
    source_ids: &[MemoryId],
    params: ConsolidateParams,
) -> Result<MemoryId> {
    let new_id = MemoryId::new();
    let now = Utc::now();

    // ── Pre-transaction: fetch all source records and derive aggregated metadata ──
    // This single DB round-trip replaces the old `fetch_source_temporal_bounds`
    // call and the caller-side metadata extraction from `first` only.
    let (meta, inherited_event_at, inherited_valid_until) =
        fetch_source_metadata(db, source_ids).await?;

    // Caller-supplied event_at takes precedence only if provided; otherwise
    // fall back to the inherited minimum, then to now.
    let event = params.event_at
        .or(inherited_event_at)
        .unwrap_or(now);

    // Consolidated memories never carry a TTL — validation::ttl_duration is
    // not called here, matching the original flow (None passed for expires_at).

    // Build multi-statement transaction SQL.
    // UPDATE indices: 2 .. 2+N
    // RELATE indices: 2+N .. 2+2N
    let n = source_ids.len();
    let mut sql = String::from("BEGIN TRANSACTION;\n");

    sql.push_str(
        "CREATE type::record('memory', $new_id) CONTENT {\
            summary: $summary, content: NONE,\
            memory_type: $memory_type, subtype: $subtype,\
            channel: $channel, modality: $modality,\
            source_ref: NONE,\
            project: $project, domain: $domain,\
            people: $people, importance: $importance,\
            visibility: $visibility, status: 'active',\
            tags: $tags, access_count: 0,\
            event_at: $event_at, created_at: $created_at,\
            updated_at: $updated_at, expires_at: NONE,\
            source_agent: $source_agent, session_id: NONE,\
            valid_until: $valid_until,\
            invalidated_at: NONE, invalidation_reason: NONE\
        };\n",
    );

    if params.vector.is_some() {
        // Immediately set the vector on the same record within the transaction.
        sql.push_str(
            "UPDATE type::record('memory', $new_id) SET vector = $vector;\n",
        );
    }

    for i in 0..n {
        // Guard `WHERE invalidated_at IS NONE` preserves the original
        // `invalidation_reason` if a source was already retracted/forgotten/
        // expired between candidate selection and the consolidation call.
        // `valid_until` mirrors archive(): set to now if not already set so
        // `recall_at(as_of)` correctly fences point-in-time queries (ADR-011).
        sql.push_str(&format!(
            "UPDATE type::record('memory', $src_str_{i}) \
             SET status = 'archived', \
                 invalidated_at = time::now(), \
                 invalidation_reason = 'consolidated', \
                 valid_until = IF valid_until IS NONE THEN time::now() ELSE valid_until END, \
                 updated_at = time::now() \
             WHERE invalidated_at IS NONE;\n"
        ));
    }

    for i in 0..n {
        // RELATE requires RecordId bindings in v3 — $new_rid and $src_rid_N
        // are bound as concrete RecordId values below.
        sql.push_str(&format!(
            "RELATE $new_rid -> derives -> $src_rid_{i};\n"
        ));
    }

    sql.push_str("COMMIT TRANSACTION;\n");

    // Capture before params is partially moved.
    let has_vector = params.vector.is_some();

    // Start binding the fixed parameters.
    let mut q = db
        .query(&sql)
        .bind(("new_id", new_id.as_str().to_string()))
        .bind(("summary", params.summary))
        .bind(("memory_type", enum_to_value(meta.memory_type)?))
        .bind(("subtype", enum_to_value(meta.subtype)?))
        .bind(("channel", meta.channel.map(|c| c.to_string())))
        .bind(("modality", meta.modality.map(|m| m.to_string())))
        .bind(("project", meta.project))
        .bind(("domain", meta.domain))
        .bind(("people", meta.people))
        .bind(("importance", meta.importance.value() as i64))
        .bind(("visibility", enum_to_value(meta.visibility)?))
        .bind(("tags", meta.tags))
        .bind(("event_at", event))
        .bind(("created_at", now))
        .bind(("updated_at", now))
        .bind(("source_agent", params.source_agent))
        .bind(("valid_until", inherited_valid_until))
        .bind(("new_rid", RecordId::new("memory", new_id.as_str())));

    if let Some(v) = params.vector {
        q = q.bind(("vector", v));
    }

    // Bind per-source parameters: string IDs for UPDATE, RecordIds for RELATE.
    for (i, src_id) in source_ids.iter().enumerate() {
        q = q
            .bind((format!("src_str_{i}"), src_id.as_str().to_string()))
            .bind((
                format!("src_rid_{i}"),
                RecordId::new("memory", src_id.as_str()),
            ));
    }

    let mut response = q
        .await
        .map_err(|e| KbError::db(format!("consolidate_atomic for new memory {}", &new_id), e))?;

    // Validate each statement's result; any embedded SurrealQL error aborts the
    // consolidation and surfaces a `KbError::Db`. The surrounding
    // `BEGIN TRANSACTION … COMMIT TRANSACTION` means a failed inner statement
    // causes the whole transaction to be rolled back — no partial commit.
    // Index layout: BEGIN(0), CREATE(1), [UPDATE_VEC(2) if vector,]
    //               UPDATE×N, RELATE×N, COMMIT(last).
    let update_offset: usize = if has_vector { 3 } else { 2 };
    let total_stmts = update_offset + n + n + 1; // +1 for COMMIT

    for idx in 0..total_stmts {
        response
            .take::<Vec<serde_json::Value>>(idx)
            .map_err(|e| KbError::db(format!("consolidate_atomic stmt {idx}"), e))?;
    }

    Ok(new_id)
}

/// Fetch all source records and compute aggregated metadata for the consolidated memory.
///
/// Aggregation rules (per plan):
/// - `importance`  = max(source.importance)
/// - `people`      = union(source.people), deduped, order-stable
/// - `tags`        = union(source.tags), deduped, order-stable
/// - `domain`      = union(source.domain), deduped, order-stable
/// - `visibility`  = most-restrictive: Sensitive > Private > Standard
///   (Sensitive sources are rejected — caller must filter them out first)
/// - `subtype`     = first source's (heuristic 1 clusters by subtype; assert not mixed)
/// - `project`     = first source's (heuristic 1 clusters by project; same reasoning)
/// - `channel`     = first source's (cluster-wide values rarely differ)
/// - `modality`    = first source's (same)
/// - `event_at`    = min(source.event_at) — returned separately for caller override
/// - `valid_until` = max(sources) only when ALL have it; else None
///
/// Returns `(SourceMetadata, min_event_at, max_valid_until)`.
async fn fetch_source_metadata(
    db: &SurrealClient,
    source_ids: &[MemoryId],
) -> Result<(SourceMetadata, Option<chrono::DateTime<Utc>>, Option<chrono::DateTime<Utc>>)> {
    if source_ids.is_empty() {
        return Err(KbError::InvalidInput(
            "consolidate_atomic requires at least one source ID".to_string(),
        ));
    }

    // Build a parameterized IN list using individual bindings.
    let placeholders: Vec<String> = (0..source_ids.len())
        .map(|i| format!("$sbnd_{i}"))
        .collect();
    let in_list = placeholders.join(", ");
    let query = format!(
        "SELECT memory_type, subtype, channel, modality, project, domain, \
                people, importance, visibility, tags, event_at, valid_until \
         FROM memory WHERE id IN [{in_list}] AND ({BITEMPORAL_ACTIVE})"
    );

    let mut q = db.query(&query);
    for (i, src_id) in source_ids.iter().enumerate() {
        q = q.bind((
            format!("sbnd_{i}"),
            surrealdb::types::RecordId::new("memory", src_id.as_str()),
        ));
    }

    let mut result = q
        .await
        .map_err(|e| KbError::db("fetch source metadata for consolidation", e))?;

    let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();

    if rows.is_empty() {
        return Err(KbError::InvalidInput(
            "no source memories found for consolidation (all missing or archived)".to_string(),
        ));
    }

    // Parse the first row for fields that are "use first source's value".
    let first = &rows[0];

    let memory_type: MemoryType = first
        .get("memory_type")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok())
        .unwrap_or(MemoryType::Semantic);

    let subtype: MemorySubtype = first
        .get("subtype")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok())
        .unwrap_or(MemorySubtype::Fact);

    let channel: Option<Channel> = first
        .get("channel")
        .filter(|v| !v.is_null())
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok());

    let modality: Option<Modality> = first
        .get("modality")
        .filter(|v| !v.is_null())
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok());

    let project: Option<String> = first
        .get("project")
        .filter(|v| !v.is_null())
        .and_then(|v| v.as_str())
        .map(String::from);

    // Aggregate across all rows.
    let mut max_importance: u8 = 1;
    let mut people_vec: Vec<String> = Vec::new();
    let mut people_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut tags_vec: Vec<String> = Vec::new();
    let mut tags_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut domain_vec: Vec<String> = Vec::new();
    let mut domain_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut max_visibility = Visibility::Standard;
    let mut min_event_at: Option<chrono::DateTime<Utc>> = None;
    let mut max_valid_until: Option<chrono::DateTime<Utc>> = None;
    let mut all_have_valid_until = true;

    for row in &rows {
        // importance: max
        let imp = row
            .get("importance")
            .and_then(|v| v.as_u64())
            .map(|v| v as u8)
            .unwrap_or(3)
            .clamp(1, 5);
        if imp > max_importance {
            max_importance = imp;
        }

        // visibility: most restrictive — Sensitive > Private > Standard
        let vis_str = row.get("visibility").and_then(|v| v.as_str()).unwrap_or("standard");
        let vis: Visibility = serde_json::from_value(serde_json::Value::String(vis_str.to_string()))
            .unwrap_or(Visibility::Standard);
        max_visibility = match (max_visibility, vis) {
            (Visibility::Sensitive, _) | (_, Visibility::Sensitive) => Visibility::Sensitive,
            (Visibility::Private, _) | (_, Visibility::Private) => Visibility::Private,
            _ => Visibility::Standard,
        };

        // people: union, insertion-order stable
        if let Some(arr) = row.get("people").and_then(|v| v.as_array()) {
            for v in arr {
                if let Some(s) = v.as_str()
                    && people_seen.insert(s.to_string())
                {
                    people_vec.push(s.to_string());
                }
            }
        }

        // tags: union, insertion-order stable
        if let Some(arr) = row.get("tags").and_then(|v| v.as_array()) {
            for v in arr {
                if let Some(s) = v.as_str()
                    && tags_seen.insert(s.to_string())
                {
                    tags_vec.push(s.to_string());
                }
            }
        }

        // domain: union, insertion-order stable
        if let Some(arr) = row.get("domain").and_then(|v| v.as_array()) {
            for v in arr {
                if let Some(s) = v.as_str()
                    && domain_seen.insert(s.to_string())
                {
                    domain_vec.push(s.to_string());
                }
            }
        }

        // event_at: min
        if let Some(ea_str) = row.get("event_at").and_then(|v| v.as_str())
            && let Ok(ea) = ea_str.parse::<chrono::DateTime<Utc>>()
        {
            min_event_at = Some(match min_event_at {
                None => ea,
                Some(prev) => prev.min(ea),
            });
        }

        // valid_until: max (only when ALL sources have it)
        let vu = row.get("valid_until").and_then(|v| {
            if v.is_null() {
                None
            } else {
                v.as_str().and_then(|s| s.parse::<chrono::DateTime<Utc>>().ok())
            }
        });
        match vu {
            None => all_have_valid_until = false,
            Some(vu_ts) => {
                max_valid_until = Some(match max_valid_until {
                    None => vu_ts,
                    Some(prev) => prev.max(vu_ts),
                });
            }
        }
    }

    // Sensitive sources must be filtered by the caller (find_candidates only returns
    // Standard visibility clusters). Reject here as a safety gate.
    if max_visibility == Visibility::Sensitive {
        return Err(KbError::InvalidInput(
            "cannot consolidate: cluster contains Sensitive memories".to_string(),
        ));
    }

    let inherited_valid_until = if all_have_valid_until { max_valid_until } else { None };

    let meta = SourceMetadata {
        memory_type,
        subtype,
        channel,
        modality,
        project,
        domain: domain_vec,
        people: people_vec,
        importance: Importance::new(max_importance)
            .unwrap_or_else(|_| Importance::new(3).unwrap()),
        visibility: max_visibility,
        tags: tags_vec,
    };

    Ok((meta, min_event_at, inherited_valid_until))
}

// ---------------------------------------------------------------------------
// Minimal memory fetch for the auto-consolidation loop
// ---------------------------------------------------------------------------

/// Minimal view of a memory needed by the auto-consolidation loop.
pub struct MemoryMinimal {
    pub id: MemoryId,
    pub summary: String,
    pub visibility: Visibility,
    pub subtype: MemorySubtype,
}

/// Fetch a minimal projection of the given memory IDs from the DB.
///
/// Returns only the fields the auto-consolidation loop needs to make
/// decisions (visibility double-check, subtype for the sidecar call).
/// Uses parameterized array bindings — no string interpolation.
///
/// Missing IDs (archived between candidate selection and this call) are
/// silently omitted from the result, matching the existing pattern in
/// `fetch_source_metadata`.
#[instrument(skip(db), fields(id_count = source_ids.len()))]
pub async fn fetch_memories_by_ids(
    db: &SurrealClient,
    source_ids: &[MemoryId],
) -> Result<Vec<MemoryMinimal>> {
    if source_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: Vec<String> = (0..source_ids.len())
        .map(|i| format!("$fmbnd_{i}"))
        .collect();
    let in_list = placeholders.join(", ");
    // BITEMPORAL_ACTIVE ensures archived memories (invalidated between candidate
    // selection and this fetch) are excluded — prevents stale data reaching the
    // sidecar call and keeps the result count consistent with the live DB state.
    let query = format!(
        "SELECT record::id(id) AS mid, summary, visibility, subtype \
         FROM memory WHERE id IN [{in_list}] AND ({BITEMPORAL_ACTIVE})"
    );

    let mut q = db.query(&query);
    for (i, src_id) in source_ids.iter().enumerate() {
        q = q.bind((
            format!("fmbnd_{i}"),
            surrealdb::types::RecordId::new("memory", src_id.as_str()),
        ));
    }

    let mut result = q
        .await
        .map_err(|e| KbError::db("fetch_memories_by_ids", e))?;

    let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let id_str = row
            .get("mid")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if id_str.is_empty() {
            continue;
        }

        let summary = row
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let visibility: Visibility = row
            .get("visibility")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok())
            .unwrap_or(Visibility::Standard);

        let subtype: MemorySubtype = row
            .get("subtype")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok())
            .unwrap_or(MemorySubtype::Fact);

        out.push(MemoryMinimal {
            id: MemoryId::from(id_str.to_string()),
            summary,
            visibility,
            subtype,
        });
    }

    Ok(out)
}
