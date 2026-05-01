//! `kb_profile` — return a user-profile snapshot: stable static facts plus
//! recent dynamic activity. Designed for fast LLM-context injection.
//!
//! ## Shape
//!
//! ```text
//! { stable_facts: [MemorySummary],   // is_static = true, top by importance
//!   recent_activity: [MemorySummary] // is_static = false, top by event_at desc
//! }
//! ```
//!
//! Visibility is enforced exactly like `kb_recall`: callers without a `project`
//! filter cannot see Sensitive memories; Private memories are surfaced only
//! when `author_filter` matches the row's author.
//!
//! Static and dynamic buckets are independent — one query each, indexed via
//! `idx_memory_is_static`. Both lists honour `BITEMPORAL_ACTIVE`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::agent;
use crate::db::client::SurrealClient;
use crate::db::search::AuthorFilter;
use crate::db::BITEMPORAL_ACTIVE;
use crate::error::{KbError, Result};
use crate::types::*;

/// Default number of static facts surfaced when caller does not specify.
const DEFAULT_STATIC_LIMIT: usize = 20;
/// Default number of recent dynamic memories.
const DEFAULT_RECENT_LIMIT: usize = 10;
/// Hard ceiling — guards against pathological caller limits.
const MAX_LIMIT: usize = 100;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProfileInput {
    /// Optional project namespace. When set, results are restricted to memories
    /// in this project (and Sensitive memories outside the project are hidden).
    pub project: Option<String>,
    /// Optional agent-author filter. `me` → user-authored only; an agent name →
    /// only that agent's memories. Mirrors `kb_recall` semantics.
    pub author_filter: Option<String>,
    /// Number of static facts (default 20, max 100).
    pub static_limit: Option<usize>,
    /// Number of recent activity items (default 10, max 100).
    pub recent_limit: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ProfileOutput {
    pub stable_facts: Vec<MemorySummary>,
    pub recent_activity: Vec<MemorySummary>,
}

#[tracing::instrument(name = "kb_profile", skip(db))]
pub async fn execute(db: &SurrealClient, input: ProfileInput) -> Result<ProfileOutput> {
    let static_limit = input.static_limit.unwrap_or(DEFAULT_STATIC_LIMIT).min(MAX_LIMIT);
    let recent_limit = input.recent_limit.unwrap_or(DEFAULT_RECENT_LIMIT).min(MAX_LIMIT);

    // Resolve author_filter once — same string convention as kb_recall ("me" |
    // agent_name). Archived agents are allowed for historical profile views.
    let resolved_author = match input.author_filter.as_deref() {
        None => AuthorFilter::None,
        Some("me") => AuthorFilter::UserOnly,
        Some(name) => {
            let ag = agent::resolve_by_name(db, name).await?.ok_or_else(|| {
                KbError::InvalidInput(format!("unknown agent: {name}"))
            })?;
            AuthorFilter::Agent(ag.id)
        }
    };

    let stable_facts = fetch_bucket(db, &input, &resolved_author, true, static_limit, "ORDER BY importance DESC, event_at DESC").await?;
    let recent_activity = fetch_bucket(db, &input, &resolved_author, false, recent_limit, "ORDER BY event_at DESC").await?;

    Ok(ProfileOutput {
        stable_facts,
        recent_activity,
    })
}

/// Fetch one bucket (static or dynamic) with visibility + author filtering.
///
/// Visibility rules mirror search.rs/status.rs:
///   - Sensitive: only when `project` is set AND matches the row's project
///   - Private: only when caller's author_filter resolves to that author
///   - Standard: always visible (subject to other filters)
async fn fetch_bucket(
    db: &SurrealClient,
    input: &ProfileInput,
    resolved_author: &AuthorFilter,
    is_static: bool,
    limit: usize,
    order_by: &str,
) -> Result<Vec<MemorySummary>> {
    let mut conditions = vec![
        BITEMPORAL_ACTIVE.to_string(),
        "is_static = $is_static".to_string(),
    ];

    if input.project.is_some() {
        conditions.push("project = $project".to_string());
        // With a project filter, Sensitive in-project is OK; Private still requires author.
    } else {
        // No project filter: hide Sensitive entirely.
        conditions.push("visibility != 'sensitive'".to_string());
    }

    // Visibility: Private requires author_filter to match the row's author.
    // When author_filter is missing, exclude private rows entirely.
    // When `UserOnly` (me), restrict to user-authored AND non-private (private
    // memories are not user-authored by construction).
    // When `Agent(id)`, restrict to that agent's authored memories — private
    // rows from that agent are visible to that agent's queries.
    match resolved_author {
        AuthorFilter::Agent(_) => {
            conditions.push("author = $author_id".to_string());
        }
        AuthorFilter::UserOnly => {
            conditions.push("author IS NONE".to_string());
            conditions.push("visibility != 'private'".to_string());
        }
        AuthorFilter::None => {
            conditions.push("visibility != 'private'".to_string());
        }
    }

    let where_clause = conditions.join(" AND ");
    let sql = format!(
        "SELECT id, summary, memory_type, subtype, project, domain, importance, event_at, tags
         FROM memory
         WHERE {where_clause}
         {order_by}
         LIMIT $limit"
    );

    let mut q = db
        .query(sql)
        .bind(("is_static", is_static))
        .bind(("limit", limit as i64));

    if let Some(p) = &input.project {
        q = q.bind(("project", p.clone()));
    }
    if let AuthorFilter::Agent(agent_id) = resolved_author {
        q = q.bind((
            "author_id",
            surrealdb::types::RecordId::new("agent", agent_id.as_str()),
        ));
    }

    let mut result = q
        .await
        .map_err(|e| KbError::db("kb_profile bucket", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("kb_profile deserialize", e))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut map = match row {
            serde_json::Value::Object(m) => m,
            _ => continue,
        };
        // Normalise the `id` field — RecordId object → bare string.
        if let Some(id_val) = map.get("id")
            && let Some(bare) = crate::db::helpers::record_id_bare(id_val, "memory")
        {
            map.insert("id".to_string(), serde_json::Value::String(bare));
        }
        match serde_json::from_value::<MemorySummary>(serde_json::Value::Object(map)) {
            Ok(s) => out.push(s),
            Err(e) => {
                tracing::warn!("kb_profile: skipping row that failed to parse: {e}");
            }
        }
    }
    Ok(out)
}
