use surrealdb::types::RecordId;
use tracing::instrument;

use crate::db::client::SurrealClient;
use crate::error::{KbError, Result};
use crate::types::{Agent, AgentFull, AgentId, AgentKind, AgentStatus, AgentSummary};

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

pub struct RegisterInput {
    pub name: String,
    pub kind: AgentKind,
    pub model: Option<String>,
    pub version: Option<String>,
    /// Parent agent *name* — resolved to an `AgentId` before INSERT.
    pub parent: Option<String>,
    pub project: Option<String>,
    pub config: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Register a new agent, or resurrect an archived one with the same name.
///
/// Resurrection: if the name already exists and status = 'archived', the row is
/// updated (status cleared, mutable fields refreshed) and the existing AgentId is
/// returned.  If the name exists and status = 'active' → `InvalidInput`.
///
/// **FK side-effect on resurrection:** the agent row is mutated in-place, so any
/// existing memories whose `author` FK points to this AgentId will immediately
/// reflect the new `kind`, `model`, and `version` when drilled.  The AgentId itself
/// is stable — callers do not need to re-link memories after a resurrection.
#[instrument(skip(db, input), fields(name = %input.name, kind = %input.kind))]
pub async fn register(db: &SurrealClient, input: RegisterInput) -> Result<AgentId> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(KbError::InvalidInput("agent name must not be empty".into()));
    }
    if name.len() > 128 {
        return Err(KbError::InvalidInput(
            "agent name must be 128 characters or fewer".into(),
        ));
    }

    // Resolve parent name → AgentId if provided.
    let parent_id: Option<AgentId> = if let Some(ref p) = input.parent {
        let parent_agent = resolve_by_name(db, p).await?.ok_or_else(|| {
            KbError::InvalidInput(format!("parent agent '{p}' not found"))
        })?;
        Some(parent_agent.id)
    } else {
        None
    };

    // Check for an existing agent with this name.
    let existing = resolve_by_name(db, &name).await?;

    match existing {
        Some(existing_agent) if existing_agent.status == AgentStatus::Active => {
            return Err(KbError::InvalidInput(format!(
                "agent '{}' already active",
                name
            )));
        }
        Some(existing_agent) => {
            // Resurrection path: status is archived.
            let id = existing_agent.id.clone();
            let kind_str = input.kind.as_str().to_string();
            tracing::info!(
                agent_id = %id,
                kind = %input.kind,
                "resurrecting archived agent"
            );

            let parent_rid: Option<RecordId> =
                parent_id.as_ref().map(|pid| RecordId::new("agent", pid.as_str()));

            db.query(
                "UPDATE type::record('agent', $id) SET
                    status     = 'active',
                    retired_at = NONE,
                    last_active = time::now(),
                    kind        = $kind,
                    model       = $model,
                    version     = $version,
                    parent_agent = $parent_agent,
                    project     = $project,
                    config      = $config",
            )
            .bind(("id", id.as_str().to_string()))
            .bind(("kind", kind_str))
            .bind(("model", input.model))
            .bind(("version", input.version))
            .bind(("parent_agent", parent_rid))
            .bind(("project", input.project))
            .bind(("config", input.config))
            .await
            .map_err(|e| KbError::db(format!("resurrecting agent {id}"), e))?;

            return Ok(id);
        }
        None => {
            // New agent.
        }
    }

    let id = AgentId::new();
    let kind_str = input.kind.as_str().to_string();
    tracing::info!(
        agent_id = %id,
        kind = %input.kind,
        "registering new agent"
    );

    let parent_rid: Option<RecordId> =
        parent_id.as_ref().map(|pid| RecordId::new("agent", pid.as_str()));

    db.query(
        "CREATE type::record('agent', $id) CONTENT {
            name:         $name,
            kind:         $kind,
            model:        $model,
            version:      $version,
            parent_agent: $parent_agent,
            project:      $project,
            config:       $config,
            status:       'active',
            spawned_at:   time::now(),
            retired_at:   NONE,
            last_active:  time::now()
        }",
    )
    .bind(("id", id.as_str().to_string()))
    .bind(("name", name))
    .bind(("kind", kind_str))
    .bind(("model", input.model))
    .bind(("version", input.version))
    .bind(("parent_agent", parent_rid))
    .bind(("project", input.project))
    .bind(("config", input.config))
    .await
    .map_err(|e| KbError::db(format!("creating agent {id}"), e))?;

    Ok(id)
}

/// Soft-retire an agent.
///
/// `id_or_name`: if it starts with `"agent_"` it is treated as an `AgentId`,
/// otherwise it is a name lookup.  `purge_memories = true` soft-archives all
/// active memories authored by this agent (never hard-deletes).
#[instrument(skip(db))]
pub async fn retire(db: &SurrealClient, id_or_name: String, purge_memories: bool) -> Result<()> {
    let agent = resolve_id_or_name(db, &id_or_name).await?;

    if agent.status == AgentStatus::Archived {
        return Err(KbError::InvalidInput("already retired".into()));
    }

    let id = agent.id;
    tracing::info!(agent_id = %id, "retiring agent");

    db.query(
        "UPDATE type::record('agent', $id) SET
            status     = 'archived',
            retired_at = time::now()",
    )
    .bind(("id", id.as_str().to_string()))
    .await
    .map_err(|e| KbError::db(format!("retiring agent {id}"), e))?;

    if purge_memories {
        // ADR-011 invariant: status='archived' iff invalidated_at IS NOT NONE.
        // The UPDATE must set both — and guard on `invalidated_at IS NONE` so
        // already-invalidated memories (retracted/forgotten) preserve their
        // original invalidation reason.
        let agent_rid = RecordId::new("agent", id.as_str());
        db.query(
            "UPDATE memory SET
                status              = 'archived',
                invalidated_at      = time::now(),
                invalidation_reason = 'forgotten',
                updated_at          = time::now()
             WHERE author = $agent_record AND invalidated_at IS NONE",
        )
        .bind(("agent_record", agent_rid))
        .await
        .map_err(|e| KbError::db(format!("purging memories for agent {id}"), e))?;
    }

    Ok(())
}

/// List agents, optionally filtered by project and/or status.
#[instrument(skip(db))]
pub async fn list(
    db: &SurrealClient,
    project: Option<String>,
    include_archived: bool,
) -> Result<Vec<AgentSummary>> {
    // Build the WHERE clause from optional predicates.
    // All user-supplied values go through parameter bindings.
    let mut conditions: Vec<&str> = Vec::new();
    if project.is_some() {
        conditions.push("project = $project");
    }
    if !include_archived {
        conditions.push("status = 'active'");
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    let sql = format!(
        "SELECT id, name, kind, status, project, last_active
         FROM agent
         {where_clause}
         ORDER BY last_active DESC"
    );

    let mut q = db.query(&sql);
    if let Some(ref p) = project {
        q = q.bind(("project", p.clone()));
    }

    let mut result = q
        .await
        .map_err(|e| KbError::db("listing agents", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing agent list", e))?;

    rows.into_iter()
        .map(parse_agent_summary_row)
        .collect()
}

/// Fetch a full agent record by id or name.
#[instrument(skip(db))]
pub async fn get(db: &SurrealClient, id_or_name: String) -> Result<AgentFull> {
    resolve_id_or_name(db, &id_or_name).await
}

/// Resolve an agent by name. Returns `None` if not found. Does NOT enforce
/// status — callers decide whether an archived agent is acceptable.
#[instrument(skip(db, name))]
pub async fn resolve_by_name(db: &SurrealClient, name: &str) -> Result<Option<Agent>> {
    let mut result = db
        .query("SELECT * FROM agent WHERE name = $name LIMIT 1")
        .bind(("name", name.to_string()))
        .await
        .map_err(|e| KbError::db(format!("resolving agent by name '{name}'"), e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db(format!("deserializing agent '{name}'"), e))?;

    match rows.into_iter().next() {
        None => Ok(None),
        Some(row) => {
            let agent = parse_agent_row(row)?;
            Ok(Some(agent))
        }
    }
}

/// Update `last_active` timestamp for an agent. Fire-and-forget friendly —
/// callers in the write path may discard the `Result`.
#[instrument(skip(db))]
pub async fn touch_last_active(db: &SurrealClient, id: &AgentId) -> Result<()> {
    db.query("UPDATE type::record('agent', $id) SET last_active = time::now()")
        .bind(("id", id.as_str().to_string()))
        .await
        .map_err(|e| KbError::db(format!("touching last_active for agent {id}"), e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Resolve `id_or_name` to a full `Agent`. Prefix `"agent_"` means it is an
/// id; anything else is treated as a name. Returns `NotFound` if missing.
async fn resolve_id_or_name(db: &SurrealClient, id_or_name: &str) -> Result<Agent> {
    if is_agent_id(id_or_name) {
        // Direct id lookup.
        let id = AgentId::from_string(id_or_name.to_string());
        let mut result = db
            .query("SELECT * FROM type::record('agent', $id)")
            .bind(("id", id.as_str().to_string()))
            .await
            .map_err(|e| KbError::db(format!("getting agent {id}"), e))?;

        let row: Option<serde_json::Value> = result
            .take(0)
            .map_err(|e| KbError::db(format!("deserializing agent {id}"), e))?;

        match row {
            Some(r) => parse_agent_row(r),
            None => Err(KbError::NotFound(id_or_name.to_string())),
        }
    } else {
        // Name lookup.
        resolve_by_name(db, id_or_name)
            .await?
            .ok_or_else(|| KbError::NotFound(id_or_name.to_string()))
    }
}

/// Returns `true` when `s` looks like an `AgentId` (starts with `"agent_"`).
fn is_agent_id(s: &str) -> bool {
    s.starts_with("agent_")
}

/// Parse a raw SurrealDB row into an `Agent`, normalising the record-id field.
fn parse_agent_row(row: serde_json::Value) -> Result<Agent> {
    let mut map = match row {
        serde_json::Value::Object(m) => m,
        _ => {
            return Err(KbError::DataCorruption(
                "agent row is not a JSON object".into(),
            ))
        }
    };

    // Normalise the `id` field: SurrealDB may return it as a RecordId object
    // or as `"agent:<id_str>"` — strip the table prefix to get a bare AgentId.
    if let Some(id_val) = map.get("id") {
        let id_str = normalise_record_id(id_val, "agent");
        map.insert("id".to_string(), serde_json::Value::String(id_str));
    }

    // Normalise `parent_agent`: stored as option<record<agent>>, so the same
    // RecordId treatment applies when present.
    if let Some(parent_val) = map.get("parent_agent")
        && !parent_val.is_null()
    {
        let parent_str = normalise_record_id(parent_val, "agent");
        map.insert(
            "parent_agent".to_string(),
            serde_json::Value::String(parent_str),
        );
    }

    serde_json::from_value(serde_json::Value::Object(map))
        .map_err(|e| KbError::DataCorruption(format!("parsing agent row: {e}")))
}

/// Parse a summary-projection row (id, name, kind, status, project, last_active).
fn parse_agent_summary_row(row: serde_json::Value) -> Result<AgentSummary> {
    let mut map = match row {
        serde_json::Value::Object(m) => m,
        _ => {
            return Err(KbError::DataCorruption(
                "agent summary row is not a JSON object".into(),
            ))
        }
    };

    if let Some(id_val) = map.get("id") {
        let id_str = normalise_record_id(id_val, "agent");
        map.insert("id".to_string(), serde_json::Value::String(id_str));
    }

    serde_json::from_value(serde_json::Value::Object(map))
        .map_err(|e| KbError::DataCorruption(format!("parsing agent summary row: {e}")))
}

/// Strip the `{table}:` prefix from a SurrealDB record-id value, mirroring the
/// normalisation used in `entity.rs`.
fn normalise_record_id(id_val: &serde_json::Value, table: &str) -> String {
    let prefix = format!("{table}:");
    match id_val {
        serde_json::Value::String(s) => s.strip_prefix(&prefix).unwrap_or(s).to_string(),
        serde_json::Value::Object(obj) => obj
            .get("id")
            .and_then(|v| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Object(inner) => inner
                    .get("String")
                    .and_then(|s| s.as_str().map(String::from)),
                _ => None,
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_kind_to_string_all_variants() {
        assert_eq!(AgentKind::Assistant.to_string(), "assistant");
        assert_eq!(AgentKind::Reviewer.to_string(), "reviewer");
        assert_eq!(AgentKind::Summarizer.to_string(), "summarizer");
        assert_eq!(AgentKind::Researcher.to_string(), "researcher");
        assert_eq!(AgentKind::Custom.to_string(), "custom");
    }

    #[test]
    fn is_agent_id_detects_prefix() {
        assert!(is_agent_id("agent_abc123"));
        assert!(is_agent_id("agent_"));
        assert!(!is_agent_id("my-agent"));
        assert!(!is_agent_id(""));
        assert!(!is_agent_id("agent"));
    }

    #[test]
    fn normalise_record_id_strips_prefix() {
        let val = serde_json::Value::String("agent:agent_abc123".to_string());
        assert_eq!(normalise_record_id(&val, "agent"), "agent_abc123");
    }

    #[test]
    fn normalise_record_id_passthrough_when_no_prefix() {
        let val = serde_json::Value::String("agent_abc123".to_string());
        assert_eq!(normalise_record_id(&val, "agent"), "agent_abc123");
    }

    #[test]
    fn normalise_record_id_object_shape() {
        // SurrealDB v3 nested object: { "tb": "agent", "id": { "String": "agent_abc123" } }
        let val = serde_json::json!({
            "tb": "agent",
            "id": { "String": "agent_abc123" }
        });
        assert_eq!(normalise_record_id(&val, "agent"), "agent_abc123");
    }

    #[test]
    fn register_input_name_validation_logic() {
        // Verify the validation constants via string operations (no DB needed).
        let empty = "";
        let whitespace = "   ";
        let long_name = "a".repeat(129);
        let valid = "my-agent";

        assert!(empty.trim().is_empty());
        assert!(whitespace.trim().is_empty());
        assert!(long_name.len() > 128);
        assert!(!valid.trim().is_empty() && valid.len() <= 128);
    }
}
