use chrono::Utc;
use surrealdb::types::RecordId;
use tracing::instrument;

use crate::db::client::SurrealClient;
use crate::db::memory::parse_memory_row;
use crate::db::BITEMPORAL_ACTIVE;
use crate::error::{KbError, Result};
use crate::types::*;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Upsert an entity by name.
///
/// If an entity with this name already exists, its fields are updated (roles
/// and kind are replaced; attrs are merged via object::merge; context is
/// replaced if provided). If no entity exists, a new one is created with a
/// fresh `EntityId`.
#[instrument(skip(db, name, attrs, context), fields(kind = %kind))]
pub async fn upsert(
    db: &SurrealClient,
    name: &str,
    kind: EntityKind,
    roles: Vec<String>,
    attrs: serde_json::Value,
    context: Option<String>,
) -> Result<EntityId> {
    if name.trim().is_empty() {
        return Err(KbError::InvalidInput("entity name must not be empty".into()));
    }

    let now = Utc::now();

    // Try to find an existing entity by name first.
    let existing = get_by_name(db, name).await;

    match existing {
        Ok(entity) => {
            // Entity exists — update it.
            db.query(
                "UPDATE type::record('entity', $id) SET
                    kind        = $kind,
                    roles       = $roles,
                    attrs       = $new_attrs,
                    context     = $context,
                    updated_at  = $now",
            )
            .bind(("id", entity.id.as_str().to_string()))
            .bind(("kind", kind.to_string()))
            .bind(("roles", roles))
            .bind(("new_attrs", attrs))
            .bind(("context", context))
            .bind(("now", now))
            .await
            .map_err(|e| KbError::db(format!("updating entity {}", entity.id), e))?;

            Ok(entity.id)
        }
        Err(KbError::NotFound(_)) => {
            // Entity does not exist — create it.
            let id = EntityId::new();

            db.query(
                "CREATE type::record('entity', $id) CONTENT {
                    name:       $name,
                    kind:       $kind,
                    roles:      $roles,
                    attrs:      $attrs,
                    context:    $context,
                    created_at: $created_at,
                    updated_at: $updated_at
                }",
            )
            .bind(("id", id.as_str().to_string()))
            .bind(("name", name.to_string()))
            .bind(("kind", kind.to_string()))
            .bind(("roles", roles))
            .bind(("attrs", attrs))
            .bind(("context", context))
            .bind(("created_at", now))
            .bind(("updated_at", now))
            .await
            .map_err(|e| KbError::db(format!("creating entity {id}"), e))?;

            Ok(id)
        }
        Err(e) => Err(e),
    }
}

/// Get a single entity by its `EntityId`. Returns NotFound if archived.
#[instrument(skip(db))]
pub async fn get(db: &SurrealClient, id: &EntityId) -> Result<Entity> {
    // Use the same record lookup pattern as memory::get
    let mut result = db
        .query("SELECT * FROM type::record('entity', $id) WHERE archived_at IS NONE")
        .bind(("id", id.as_str().to_string()))
        .await
        .map_err(|e| KbError::db(format!("getting entity {id}"), e))?;

    let row: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db(format!("deserializing entity {id}"), e))?;

    if let Some(row) = row {
        return parse_entity_row(row, id.as_str());
    }

    // Fallback: query by record::id match (also filters archived)
    let mut result = db
        .query("SELECT * FROM entity WHERE record::id(id) = $id AND archived_at IS NONE LIMIT 1")
        .bind(("id", id.as_str().to_string()))
        .await
        .map_err(|e| KbError::db(format!("getting entity {id} (fallback)"), e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db(format!("deserializing entity {id}"), e))?;

    let row = rows.into_iter().next()
        .ok_or_else(|| KbError::NotFound(id.to_string()))?;
    parse_entity_row(row, id.as_str())
}

/// Get a single entity by its unique name. Returns NotFound if archived.
#[instrument(skip(db, name))]
pub async fn get_by_name(db: &SurrealClient, name: &str) -> Result<Entity> {
    let mut result = db
        .query("SELECT * FROM entity WHERE name = $name AND archived_at IS NONE LIMIT 1")
        .bind(("name", name.to_string()))
        .await
        .map_err(|e| KbError::db(format!("getting entity by name '{name}'"), e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db(format!("deserializing entity '{name}'"), e))?;

    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| KbError::NotFound(name.to_string()))?;

    // Extract the normalised id string from the row before calling parse.
    let raw_id = extract_id_str(&row).unwrap_or_else(|| name.to_string());
    parse_entity_row(row, &raw_id)
}

/// Get an entity with its related memory count and the 5 most recent active
/// memory summaries (via the `involves` graph edge).
#[instrument(skip(db))]
pub async fn get_full(db: &SurrealClient, id: &EntityId) -> Result<EntityFull> {
    // 1. Fetch the base entity.
    let entity = get(db, id).await?;

    // 2. Count active memories linked via involves.
    let memory_count = count_memories(db, id).await?;

    // 3. Fetch up to 5 recent active memory summaries.
    let recent_memories = recent_memories(db, id, 5).await?;

    Ok(EntityFull {
        id: entity.id,
        name: entity.name,
        kind: entity.kind,
        roles: entity.roles,
        attrs: entity.attrs,
        context: entity.context,
        memory_count,
        recent_memories,
        created_at: entity.created_at,
        updated_at: entity.updated_at,
    })
}

/// Search entities by name (case-insensitive substring), with optional
/// `kind` and `role` filters. Returns `EntitySummary` rows ordered by
/// descending memory count.
#[instrument(skip(db, query))]
pub async fn find(
    db: &SurrealClient,
    query: &str,
    kind: Option<EntityKind>,
    role: Option<&str>,
) -> Result<Vec<EntitySummary>> {
    let mut conditions = vec![
        "string::lowercase(name) CONTAINS string::lowercase($query)".to_string(),
        "archived_at IS NONE".to_string(),
    ];
    if kind.is_some() {
        conditions.push("kind = $kind".to_string());
    }
    if role.is_some() {
        conditions.push("$role INSIDE roles".to_string());
    }
    let where_sql = conditions.join(" AND ");

    let sql = format!(
        "SELECT
            id,
            name,
            kind,
            roles,
            context,
            count(<-involves<-memory[WHERE {BITEMPORAL_ACTIVE}]) AS memory_count
         FROM entity
         WHERE {where_sql}
         ORDER BY memory_count DESC
         LIMIT 20"
    );

    let mut q = db.query(&sql).bind(("query", query.to_string()));
    if let Some(k) = kind {
        q = q.bind(("kind", k.as_str().to_string()));
    }
    if let Some(r) = role {
        q = q.bind(("role", r.to_string()));
    }

    let mut result = q
        .await
        .map_err(|e| KbError::db(format!("finding entities matching '{query}'"), e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing entity search results", e))?;

    let summaries = rows
        .into_iter()
        .filter_map(|v| parse_entity_summary_row(v).ok())
        .collect();

    Ok(summaries)
}

/// Soft-archive an entity by setting `archived_at` and `archive_reason`.
///
/// Idempotent: returns Ok(false) if already archived or not found.
/// Returns Ok(true) if the entity was archived by this call.
#[instrument(skip(db))]
pub async fn archive(db: &SurrealClient, id: &EntityId, reason: Option<&str>) -> Result<bool> {
    let mut response = db
        .query(
            "UPDATE entity
             SET archived_at    = time::now(),
                 archive_reason = $reason
             WHERE record::id(id) = $id AND archived_at IS NONE",
        )
        .bind(("id", id.as_str().to_string()))
        .bind(("reason", reason.map(str::to_string)))
        .await
        .map_err(|e| KbError::db(format!("archiving entity {id}"), e))?;

    let updated: Vec<serde_json::Value> = response.take(0usize).unwrap_or_default();
    Ok(!updated.is_empty())
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Count active memories linked to this entity via the `involves` edge.
async fn count_memories(db: &SurrealClient, id: &EntityId) -> Result<u64> {
    let ent_rid = RecordId::new("entity", id.as_str());

    let mut result = db
        .query(format!(
            "SELECT count(<-involves<-memory[WHERE {BITEMPORAL_ACTIVE}]) AS total
             FROM $entity"
        ))
        .bind(("entity", ent_rid))
        .await
        .map_err(|e| KbError::db(format!("counting memories for entity {id}"), e))?;

    let row: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing memory count", e))?;

    Ok(row
        .and_then(|v| v.get("total").and_then(|t| t.as_u64()))
        .unwrap_or(0))
}

/// Fetch the `n` most recent active memory summaries linked via `involves`.
async fn recent_memories(
    db: &SurrealClient,
    id: &EntityId,
    n: i64,
) -> Result<Vec<MemorySummary>> {
    let ent_rid = RecordId::new("entity", id.as_str());

    let mut result = db
        .query(format!(
            "SELECT *
             FROM memory
             WHERE id IN (SELECT in FROM involves WHERE out = $entity).in
               AND {BITEMPORAL_ACTIVE}
               AND visibility = 'standard'
             ORDER BY event_at DESC
             LIMIT $n"
        ))
        .bind(("entity", ent_rid))
        .bind(("n", n))
        .await
        .map_err(|e| KbError::db(format!("fetching recent memories for entity {id}"), e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing recent memories", e))?;

    // Re-use parse_memory_row to normalise each row, then convert to summary.
    let summaries = rows
        .into_iter()
        .filter_map(|row| {
            // Extract id string for parse_memory_row's second argument.
            let raw_id = extract_id_str(&row)
                .map(MemoryId::from)
                .unwrap_or_default();
            parse_memory_row(row, &raw_id)
                .ok()
                .map(MemorySummary::from)
        })
        .collect();

    Ok(summaries)
}

/// Normalise an entity row from SurrealDB into an `Entity`.
///
/// Mirrors the id-normalisation logic in `memory::parse_memory_row` but for
/// the `entity:` table prefix.
pub fn parse_entity_row(row: serde_json::Value, fallback_id: &str) -> Result<Entity> {
    let mut map = match row {
        serde_json::Value::Object(m) => m,
        _ => return Err(KbError::NotFound(fallback_id.to_string())),
    };

    // Normalise the `id` field to a plain string (strip `entity:` prefix).
    let id_str = if let Some(id_val) = map.get("id") {
        normalise_record_id(id_val, "entity", fallback_id)
    } else {
        fallback_id.to_string()
    };
    map.insert(
        "id".to_string(),
        serde_json::Value::String(id_str),
    );

    // Ensure `attrs` is always an object, never null.
    if map.get("attrs").is_none_or(|v| v.is_null()) {
        map.insert("attrs".to_string(), serde_json::json!({}));
    }

    serde_json::from_value(serde_json::Value::Object(map))
        .map_err(|e| KbError::DataCorruption(format!("parsing entity '{fallback_id}': {e}")))
}

/// Normalise an entity summary row (includes `memory_count` from graph query).
fn parse_entity_summary_row(row: serde_json::Value) -> Result<EntitySummary> {
    let mut map = match row {
        serde_json::Value::Object(m) => m,
        _ => return Err(KbError::InvalidInput("entity summary row is not an object".into())),
    };

    let fallback = String::new();
    let id_str = if let Some(id_val) = map.get("id") {
        normalise_record_id(id_val, "entity", &fallback)
    } else {
        fallback.clone()
    };
    map.insert(
        "id".to_string(),
        serde_json::Value::String(id_str),
    );

    serde_json::from_value(serde_json::Value::Object(map))
        .map_err(|e| KbError::InvalidInput(format!("parsing entity summary: {e}")))
}

/// Extract the id string from a raw SurrealDB row (pre-normalisation).
///
/// Used when we need the id string before calling a parse function that
/// accepts a fallback.
fn extract_id_str(row: &serde_json::Value) -> Option<String> {
    let id_val = row.get("id")?;
    Some(normalise_record_id(id_val, "entity", ""))
}

/// Convert a SurrealDB record-id value to a plain string, stripping the
/// `{table}:` prefix when present, or falling back to the supplied id.
fn normalise_record_id(id_val: &serde_json::Value, table: &str, fallback: &str) -> String {
    crate::db::helpers::record_id_bare(id_val, table).unwrap_or_else(|| fallback.to_string())
}
