use chrono::Utc;
use tracing::instrument;

use crate::db::client::SurrealClient;
use crate::db::helpers::enum_to_value;
use crate::db::validation;
use crate::error::{KbError, Result};
use crate::types::*;

/// Create a new memory record.
#[allow(clippy::too_many_arguments)]
#[instrument(skip(db, summary, content), fields(memory_type = %memory_type, subtype = %subtype))]
pub async fn create(
    db: &SurrealClient,
    summary: String,
    content: Option<String>,
    memory_type: MemoryType,
    subtype: MemorySubtype,
    source: Source,
    project: Option<String>,
    domain: Vec<String>,
    people: Vec<String>,
    importance: Importance,
    visibility: Visibility,
    event_at: Option<chrono::DateTime<chrono::Utc>>,
    tags: Vec<String>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    source_agent: Option<String>,
    session_id: Option<String>,
) -> Result<MemoryId> {
    if !subtype.valid_for(memory_type) {
        return Err(KbError::InvalidInput(format!(
            "subtype {subtype} is not valid for type {memory_type}"
        )));
    }

    let id = MemoryId::new();
    let now = Utc::now();
    let event = event_at.unwrap_or(now);

    // Calculate TTL-based expiry if not explicitly set (single source: validation::ttl_duration)
    let expires = expires_at.or_else(|| validation::ttl_duration(memory_type, subtype).map(|d| now + d));

    db.query(
        "CREATE type::record($table, $id) CONTENT {
            summary: $summary,
            content: $content,
            memory_type: $memory_type,
            subtype: $subtype,
            source: $source,
            source_ref: NONE,
            project: $project,
            domain: $domain,
            people: $people,
            importance: $importance,
            visibility: $visibility,
            status: 'active',
            tags: $tags,
            access_count: 0,
            event_at: $event_at,
            created_at: $created_at,
            updated_at: $updated_at,
            expires_at: $expires_at,
            source_agent: $source_agent,
            session_id: $session_id
        }"
    )
    .bind(("table", "memory"))
    .bind(("id", id.as_str().to_string()))
    .bind(("summary", summary))
    .bind(("content", content))
    .bind(("memory_type", enum_to_value(memory_type)?))
    .bind(("subtype", enum_to_value(subtype)?))
    .bind(("source", enum_to_value(source)?))
    .bind(("project", project))
    .bind(("domain", domain))
    .bind(("people", people))
    .bind(("importance", importance.value() as i64))
    .bind(("visibility", enum_to_value(visibility)?))
    .bind(("tags", tags))
    .bind(("event_at", event))
    .bind(("created_at", now))
    .bind(("updated_at", now))
    .bind(("expires_at", expires))
    .bind(("source_agent", source_agent))
    .bind(("session_id", session_id))
    .await
    .map_err(|e| KbError::db(format!("creating memory {}", &id), e))?;

    Ok(id)
}

/// Get a single memory by ID.
#[instrument(skip(db))]
pub async fn get(db: &SurrealClient, id: &MemoryId) -> Result<Memory> {
    let mut result = db
        .query("SELECT * FROM type::record('memory', $id)")
        .bind(("id", id.as_str().to_string()))
        .await
        .map_err(|e| KbError::db(format!("getting memory {id}"), e))?;

    let row: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db(format!("deserializing memory {id}"), e))?;

    let row = row.ok_or_else(|| KbError::NotFound(id.to_string()))?;
    parse_memory_row(row, id)
}

/// Get full memory and increment access_count.
#[instrument(skip(db))]
pub async fn get_and_touch(db: &SurrealClient, id: &MemoryId) -> Result<Memory> {
    db.query(
        "UPDATE type::record('memory', $id) SET access_count += 1, updated_at = time::now()"
    )
    .bind(("id", id.as_str().to_string()))
    .await
    .map_err(|e| KbError::db(format!("touching memory {id}"), e))?;

    get(db, id).await
}

/// Mark a memory as archived (soft delete).
#[instrument(skip(db))]
pub async fn archive(db: &SurrealClient, id: &MemoryId) -> Result<()> {
    let mut result = db
        .query(
            "UPDATE type::record('memory', $id) SET status = 'archived', updated_at = time::now() RETURN BEFORE"
        )
        .bind(("id", id.as_str().to_string()))
        .await
        .map_err(|e| KbError::db(format!("archiving memory {id}"), e))?;

    let before: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db(format!("checking memory {id} exists"), e))?;

    if before.is_none() {
        return Err(KbError::NotFound(id.to_string()));
    }
    Ok(())
}

/// Atomically mark a memory as superseded, only if it is currently active.
///
/// Returns `true` if the supersession succeeded (status was active),
/// `false` if the memory was already superseded/archived by a concurrent call.
#[instrument(skip(db))]
pub async fn supersede(db: &SurrealClient, id: &MemoryId) -> Result<bool> {
    let mut result = db
        .query(
            "UPDATE type::record('memory', $id)
             SET status = 'superseded', updated_at = time::now()
             WHERE status = 'active'
             RETURN BEFORE"
        )
        .bind(("id", id.as_str().to_string()))
        .await
        .map_err(|e| KbError::db(format!("superseding memory {id}"), e))?;

    let before: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db(format!("checking supersede result for {id}"), e))?;

    // If RETURN BEFORE is None, the conditional UPDATE matched no rows —
    // either the ID doesn't exist or status was already non-active.
    Ok(before.is_some())
}

/// Promote a memory: clear expiry, bump importance to at least 3.
#[instrument(skip(db))]
pub async fn promote(db: &SurrealClient, id: &MemoryId) -> Result<()> {
    let mem = get(db, id).await?;
    let new_importance = std::cmp::max(mem.importance.value(), 3) as i64;

    db.query(
        "UPDATE type::record('memory', $id) SET
            expires_at = NONE,
            importance = $importance,
            updated_at = time::now()"
    )
    .bind(("id", id.as_str().to_string()))
    .bind(("importance", new_importance))
    .await
    .map_err(|e| KbError::db(format!("promoting memory {id}"), e))?;
    Ok(())
}

/// Count all memories, optionally filtered by status.
pub async fn count(db: &SurrealClient, status: Option<MemoryStatus>) -> Result<u64> {
    let (query, binds) = match status {
        Some(s) => {
            let sv = enum_to_value(s)?;
            ("SELECT count() AS total FROM memory WHERE status = $status GROUP ALL", Some(sv))
        }
        None => ("SELECT count() AS total FROM memory GROUP ALL", None),
    };

    let mut result = if let Some(sv) = binds {
        db.query(query).bind(("status", sv)).await
    } else {
        db.query(query).await
    }
    .map_err(|e| KbError::db("counting memories", e))?;

    let row: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing count", e))?;

    Ok(row
        .and_then(|v| v.get("total").and_then(|t| t.as_u64()))
        .unwrap_or(0))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn parse_memory_row(row: serde_json::Value, id: &MemoryId) -> Result<Memory> {
    // SurrealDB returns `id` as a RecordId object; we normalize to our MemoryId.
    let mut map = match row {
        serde_json::Value::Object(m) => m,
        _ => return Err(KbError::NotFound(id.to_string())),
    };

    // Normalize the id field to a plain string
    if let Some(id_val) = map.get("id") {
        let id_str = match id_val {
            serde_json::Value::String(s) => {
                // Strip "memory:" prefix if present
                s.strip_prefix("memory:").unwrap_or(s).to_string()
            }
            serde_json::Value::Object(obj) => {
                // SurrealDB record id object: { "tb": "memory", "id": { "String": "..." } }
                obj.get("id")
                    .and_then(|v| match v {
                        serde_json::Value::String(s) => Some(s.clone()),
                        serde_json::Value::Object(inner) => {
                            inner.get("String").and_then(|s| s.as_str().map(String::from))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| id.as_str().to_string())
            }
            _ => id.as_str().to_string(),
        };
        map.insert("id".to_string(), serde_json::Value::String(id_str));
    }

    // Wrap importance int into our newtype format
    if let Some(imp) = map.get("importance").and_then(|v| v.as_u64()) {
        map.insert("importance".to_string(), serde_json::json!(imp));
    }

    // Default new fields for backward compat with existing records
    if !map.contains_key("domain") {
        map.insert("domain".to_string(), serde_json::json!([]));
    }
    if !map.contains_key("visibility") {
        map.insert("visibility".to_string(), serde_json::json!("standard"));
    }
    if !map.contains_key("source_agent") {
        map.insert("source_agent".to_string(), serde_json::Value::Null);
    }
    if !map.contains_key("session_id") {
        map.insert("session_id".to_string(), serde_json::Value::Null);
    }

    serde_json::from_value(serde_json::Value::Object(map)).map_err(|e| {
        KbError::DataCorruption(format!("parsing memory {id}: {e}"))
    })
}
