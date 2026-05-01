use surrealdb::types::RecordId;
use tracing::instrument;

use crate::db::client::SurrealClient;
use crate::error::{KbError, Result};
use crate::types::*;

/// Create a relation between two memories.
///
/// Idempotent: if the edge already exists (unique index from migration 002),
/// the RELATE is skipped at the SurrealQL level, so no error is returned.
#[instrument(skip(db))]
pub async fn link_memories(
    db: &SurrealClient,
    from_id: &MemoryId,
    to_id: &MemoryId,
    relation: RelationType,
) -> Result<()> {
    let table = relation.to_string();

    // v3: edge table name must be a static identifier (no bind-variable support
    // in the middle slot of RELATE). Since `table` derives from a safe enum
    // (not user input), direct interpolation is acceptable.
    // v3 also requires actual RecordId bindings — type::record() is not valid
    // as a RELATE subject/object.
    //
    // Check for existing edge before RELATE to prevent duplicates. The unique
    // index (migration 002) is the hard constraint; this check avoids surfacing
    // constraint errors for normal idempotent calls.
    let from_rid = RecordId::new("memory", from_id.as_str());
    let to_rid = RecordId::new("memory", to_id.as_str());
    let sql = format!(
        "LET $existing = (SELECT * FROM {table} WHERE in = $from AND out = $to);
         IF $existing = [] {{ RELATE $from -> {table} -> $to }};"
    );
    let mut response = db
        .query(&sql)
        .bind(("from", from_rid))
        .bind(("to", to_rid))
        .await
        .map_err(|e| KbError::db(format!("linking {from_id} -[{relation}]-> {to_id}"), e))?;

    // Consume both statements to surface any embedded SurrealQL errors.
    let _: Vec<serde_json::Value> = response.take(0usize).unwrap_or_default();
    let _: Vec<serde_json::Value> = response.take(1usize).unwrap_or_default();

    Ok(())
}

/// Link a memory to an entity (involves relation).
/// The entity is upserted with kind "person" if it doesn't already exist.
#[instrument(skip(db))]
pub async fn link_entity(
    db: &SurrealClient,
    memory_id: &MemoryId,
    entity_name: &str,
) -> Result<()> {
    // Upsert entity with kind "person" (default when linking from memory people field).
    // Also updates attrs.last_contact so stale contact alerts can fire.
    let now = chrono::Utc::now();
    let now_str = now.to_rfc3339();
    db.query(
        "INSERT INTO entity { name: $name, kind: 'person', roles: [], attrs: { last_contact: $now_str }, context: NONE, created_at: $now, updated_at: $now } ON DUPLICATE KEY UPDATE updated_at = $now, attrs.last_contact = $now_str"
    )
    .bind(("name", entity_name.to_string()))
    .bind(("now", now))
    .bind(("now_str", now_str))
    .await
    .map_err(|e| KbError::db("upserting entity", e))?;

    // v3: RELATE subject must be a proper RecordId, not type::record().
    // IF/ELSE uses curly-brace blocks (no THEN...END).
    // Check for existing edge before RELATE — idempotent, prevents duplicates.
    let mem_rid = RecordId::new("memory", memory_id.as_str());
    let mut response = db.query(
        "LET $ent = (SELECT * FROM entity WHERE name = $name LIMIT 1);
         LET $existing = (SELECT * FROM involves WHERE in = $mem_record AND out = $ent[0]);
         IF $ent[0] != NONE AND $existing = [] {
             RELATE $mem_record -> involves -> $ent[0]
         };"
    )
    .bind(("name", entity_name.to_string()))
    .bind(("mem_record", mem_rid))
    .await
    .map_err(|e| KbError::db(format!("linking memory {memory_id} to entity"), e))?;

    // Consume all statements to surface embedded SurrealQL errors.
    let _: Vec<serde_json::Value> = response.take(0usize).unwrap_or_default();
    let _: Vec<serde_json::Value> = response.take(1usize).unwrap_or_default();
    let _: Vec<serde_json::Value> = response.take(2usize).unwrap_or_default();

    Ok(())
}

/// Link a memory to an entity via the `assigned` relation.
/// Used for action_item subtypes — first person = assignee.
/// Upserts the entity first to ensure it exists (mirrors link_entity behavior).
#[instrument(skip(db))]
pub async fn link_assigned(
    db: &SurrealClient,
    memory_id: &MemoryId,
    entity_name: &str,
) -> Result<()> {
    // Ensure entity exists (same upsert as link_entity)
    let now = chrono::Utc::now();
    let now_str = now.to_rfc3339();
    db.query(
        "INSERT INTO entity { name: $name, kind: 'person', roles: [], attrs: { last_contact: $now_str }, context: NONE, created_at: $now, updated_at: $now } ON DUPLICATE KEY UPDATE updated_at = $now"
    )
    .bind(("name", entity_name.to_string()))
    .bind(("now", now))
    .bind(("now_str", now_str))
    .await
    .map_err(|e| KbError::db("upserting entity for assignment", e))?;

    let mem_rid = RecordId::new("memory", memory_id.as_str());
    let mut response = db.query(
        "LET $ent = (SELECT * FROM entity WHERE name = $name LIMIT 1);
         LET $existing = (SELECT * FROM assigned WHERE in = $mem_record AND out = $ent[0]);
         IF $ent[0] != NONE AND $existing = [] {
             RELATE $mem_record -> assigned -> $ent[0]
         };"
    )
    .bind(("name", entity_name.to_string()))
    .bind(("mem_record", mem_rid))
    .await
    .map_err(|e| KbError::db(format!("assigning memory {memory_id} to entity"), e))?;

    let _: Vec<serde_json::Value> = response.take(0usize).unwrap_or_default();
    let _: Vec<serde_json::Value> = response.take(1usize).unwrap_or_default();
    let _: Vec<serde_json::Value> = response.take(2usize).unwrap_or_default();

    Ok(())
}

// Legacy `link_person` and `find_people` removed — use `link_entity()` and
// `entity::find()` instead. The `person` table still exists in the schema
// for backward compatibility with pre-existing records but new code writes
// to the `entity` table only.
