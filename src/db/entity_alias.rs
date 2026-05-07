//! Alias resolution for entities (ADR-030).
//!
//! Each alias is a row in the `entity_alias` table with an `entity_id` field
//! linking back to the owning entity.  The DB-level UNIQUE index on `value` is
//! the collision invariant; application code handles the resulting error and
//! maps it to `KbError::AliasCollision`.
//!
//! The original RELATE-based design (`RELATE entity -> entity_alias -> none`)
//! was abandoned because SurrealDB v3 treats `none` as a reserved keyword (null
//! literal), causing a parse error.  A plain SCHEMAFULL table with a
//! `record<entity>` field is equivalent for our access patterns.
//!
//! `value` is also a reserved keyword in SurrealDB v3 (the single-field
//! projection shorthand).  All SurrealQL in this module backtick-escapes every
//! occurrence: `` `value` ``.

use chrono::{DateTime, Utc};
use surrealdb::types::RecordId;
use tracing::{info, instrument, warn};

use crate::db::client::SurrealClient;
use crate::error::{KbError, Result};
use crate::types::{AgentId, EntityId};

// ── helpers ──────────────────────────────────────────────────────────────────

/// Normalise an alias value: trim whitespace, lowercase.
fn normalise(value: &str) -> String {
    value.trim().to_lowercase()
}

/// Validate a normalised alias value (len ≥ 3).
fn validate(normalised: &str) -> Result<()> {
    if normalised.len() < 3 {
        return Err(KbError::InvalidInput(
            "alias must be at least 3 characters after trimming".into(),
        ));
    }
    Ok(())
}

/// Extract the canonical entity name from a DB lookup by entity ID.
///
/// Falls back to the raw entity ID string when the name cannot be resolved.
async fn lookup_entity_name(db: &SurrealClient, entity_id_str: &str) -> String {
    let r = db
        .query("SELECT name FROM type::record('entity', $eid) LIMIT 1")
        .bind(("eid", entity_id_str.to_string()))
        .await;
    match r {
        Ok(mut res) => {
            let rows: Vec<serde_json::Value> = res.take(0).unwrap_or_default();
            rows.into_iter()
                .next()
                .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(String::from))
                .unwrap_or_else(|| entity_id_str.to_string())
        }
        Err(_) => entity_id_str.to_string(),
    }
}

// ── public API ────────────────────────────────────────────────────────────────

/// Add an alias to an entity.
///
/// The `value` is lowercased and trimmed before storage. DB-level UNIQUE on
/// `idx_alias_value_unique` prevents races — a collision maps to
/// `KbError::AliasCollision`.
///
/// Aliases identical to the entity's canonical name (after normalisation) are
/// rejected silently (not an error; callers need not pre-filter).
#[instrument(skip(db), fields(entity_id = %entity_id))]
pub async fn upsert_alias(
    db: &SurrealClient,
    entity_id: &EntityId,
    value: &str,
    added_by: Option<&AgentId>,
) -> Result<()> {
    let norm = normalise(value);
    validate(&norm)?;

    // Check that this alias isn't already active for THIS entity (idempotent).
    let existing_self: Vec<serde_json::Value> = db
        .query(
            "SELECT id FROM entity_alias
             WHERE entity_id = type::record('entity', $eid)
               AND `value` = $val
               AND archived_at IS NONE
             LIMIT 1",
        )
        .bind(("eid", entity_id.as_str().to_string()))
        .bind(("val", norm.clone()))
        .await
        .map_err(|e| KbError::db("checking existing alias for entity", e))?
        .take(0)
        .unwrap_or_default();

    if !existing_self.is_empty() {
        // Already present for this entity — idempotent success.
        return Ok(());
    }

    let ent_rid = RecordId::new("entity", entity_id.as_str());
    let added_by_rid: Option<RecordId> = added_by.map(|a| RecordId::new("agent", a.as_str()));

    // CREATE entity_alias SET entity_id = $entity, `value` = $val, ...
    // If UNIQUE index fires (duplicate value on a different entity), SurrealDB
    // returns the error embedded in the Response (not as a top-level Err from .await).
    // We must call .take(0) to surface it, then inspect the error message.
    let mut response = db
        .query(
            "CREATE entity_alias SET
                 entity_id   = $entity,
                 `value`     = $val,
                 valid_from  = time::now(),
                 added_by    = $added_by,
                 archived_at = NONE,
                 valid_until = NONE",
        )
        .bind(("entity", ent_rid))
        .bind(("val", norm.clone()))
        .bind(("added_by", added_by_rid))
        .await
        .map_err(|e| KbError::db("creating entity alias (transport)", e))?;

    // Extract the result — UNIQUE violations surface here as Err, not from .await.
    match response.take::<Vec<serde_json::Value>>(0) {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            // SurrealDB UNIQUE index violation text varies by version; match broadly.
            if msg.contains("already") || msg.contains("unique") || msg.contains("Already") || msg.contains("index") {
                // Look up which entity currently holds this alias for the error message.
                let canonical = find_alias_owner_name(db, &norm).await;
                Err(KbError::AliasCollision {
                    value: norm,
                    existing_canonical: canonical,
                })
            } else {
                Err(KbError::db("creating entity alias", e))
            }
        }
    }
}

/// Find the canonical entity name that currently owns an alias value.
///
/// Returns the entity name or the raw id string when unavailable.
async fn find_alias_owner_name(db: &SurrealClient, norm: &str) -> String {
    let mut r = match db
        .query(
            "SELECT record::id(entity_id) AS eid FROM entity_alias
             WHERE `value` = $val AND archived_at IS NONE LIMIT 1",
        )
        .bind(("val", norm.to_string()))
        .await
    {
        Ok(r) => r,
        Err(_) => return norm.to_string(),
    };
    let rows: Vec<serde_json::Value> = r.take(0).unwrap_or_default();
    let eid_str = rows
        .into_iter()
        .next()
        .and_then(|v| v.get("eid").and_then(|s| s.as_str()).map(String::from))
        .unwrap_or_else(|| norm.to_string());

    lookup_entity_name(db, &eid_str).await
}

/// Soft-archive an alias (set `archived_at` and `valid_until`).
///
/// Idempotent: returns Ok(()) if not found or already archived.
#[instrument(skip(db), fields(entity_id = %entity_id))]
pub async fn archive_alias(
    db: &SurrealClient,
    entity_id: &EntityId,
    value: &str,
) -> Result<()> {
    let norm = normalise(value);
    db.query(
        "UPDATE entity_alias
         SET archived_at = time::now(),
             valid_until = time::now()
         WHERE entity_id = type::record('entity', $eid)
           AND `value` = $val
           AND archived_at IS NONE",
    )
    .bind(("eid", entity_id.as_str().to_string()))
    .bind(("val", norm))
    .await
    .map_err(|e| KbError::db("archiving entity alias", e))?;
    Ok(())
}

/// Archive all active aliases for an entity (called when the entity itself is archived).
#[instrument(skip(db), fields(entity_id = %entity_id))]
pub async fn archive_all_for_entity(db: &SurrealClient, entity_id: &EntityId) -> Result<()> {
    db.query(
        "UPDATE entity_alias
         SET archived_at = time::now(),
             valid_until = time::now()
         WHERE entity_id = type::record('entity', $eid)
           AND archived_at IS NONE",
    )
    .bind(("eid", entity_id.as_str().to_string()))
    .await
    .map_err(|e| KbError::db("cascade-archiving aliases for entity", e))?;
    Ok(())
}

/// Return the active alias values for an entity.
#[instrument(skip(db), fields(entity_id = %entity_id))]
pub async fn aliases_for(db: &SurrealClient, entity_id: &EntityId) -> Result<Vec<String>> {
    let mut result = db
        .query(
            "SELECT `value` FROM entity_alias
             WHERE entity_id = type::record('entity', $eid)
               AND archived_at IS NONE
               AND (valid_until IS NONE OR valid_until > time::now())",
        )
        .bind(("eid", entity_id.as_str().to_string()))
        .await
        .map_err(|e| KbError::db("fetching aliases for entity", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing alias rows", e))?;

    Ok(rows
        .into_iter()
        .filter_map(|v| v.get("value").and_then(|s| s.as_str()).map(String::from))
        .collect())
}

/// Exact-match alias resolution: return the entity IDs that own this alias value.
///
/// The UNIQUE index means at most one result in normal operation; returns a Vec
/// for future-proofing.  Active aliases only.
#[instrument(skip(db))]
pub async fn resolve(db: &SurrealClient, value: &str) -> Result<Vec<EntityId>> {
    let norm = normalise(value);
    let mut result = db
        .query(
            "SELECT record::id(entity_id) AS eid FROM entity_alias
             WHERE `value` = $val
               AND archived_at IS NONE
               AND (valid_until IS NONE OR valid_until > time::now())",
        )
        .bind(("val", norm))
        .await
        .map_err(|e| KbError::db("resolving alias", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing alias resolve rows", e))?;

    Ok(rows
        .into_iter()
        .filter_map(|v| {
            v.get("eid")
                .and_then(|s| s.as_str())
                .map(|s| EntityId::from(s.to_string()))
        })
        .collect())
}

/// FTS alias resolution: BM25 token match via `idx_alias_value_ft`.
///
/// Used for fuzzy/token-level matching (e.g., "Bob" against aliases stored as tokens).
/// Active aliases only. Bounded by `limit`.
#[instrument(skip(db))]
pub async fn resolve_fts(
    db: &SurrealClient,
    value: &str,
    limit: usize,
) -> Result<Vec<EntityId>> {
    let norm = normalise(value);
    let lim = limit as i64;
    let mut result = db
        .query(
            "SELECT record::id(entity_id) AS eid FROM entity_alias
             WHERE `value` @@ $val
               AND archived_at IS NONE
               AND (valid_until IS NONE OR valid_until > time::now())
             LIMIT $lim",
        )
        .bind(("val", norm))
        .bind(("lim", lim))
        .await
        .map_err(|e| KbError::db("FTS alias resolution", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing FTS alias resolve rows", e))?;

    let ids: Vec<EntityId> = rows
        .into_iter()
        .filter_map(|v| {
            v.get("eid")
                .and_then(|s| s.as_str())
                .map(|s| EntityId::from(s.to_string()))
        })
        .collect();

    // Dedup by id string while preserving order.
    let mut seen = std::collections::HashSet::new();
    Ok(ids.into_iter().filter(|id| seen.insert(id.as_str().to_string())).collect())
}

/// Bitemporal alias resolution: active at a specific `as_of` timestamp.
///
/// Alias is active at `as_of` if:
///   `valid_from <= as_of AND (valid_until IS NONE OR valid_until > as_of) AND archived_at IS NONE`
///
/// TODO(ADR-030): wire when kb_recall_at gains a `person=` filter parameter.
/// Currently `recall_at` has no person= parameter so this function is not called
/// from the search path. It is retained for future use and tested directly.
#[instrument(skip(db))]
pub async fn resolve_at(
    db: &SurrealClient,
    value: &str,
    as_of: &DateTime<Utc>,
) -> Result<Vec<EntityId>> {
    let norm = normalise(value);
    let mut result = db
        .query(
            "SELECT record::id(entity_id) AS eid FROM entity_alias
             WHERE `value` = $val
               AND valid_from <= $as_of
               AND (valid_until IS NONE OR valid_until > $as_of)
               AND archived_at IS NONE",
        )
        .bind(("val", norm))
        .bind(("as_of", *as_of))
        .await
        .map_err(|e| KbError::db("bitemporal alias resolution", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing bitemporal alias resolve rows", e))?;

    Ok(rows
        .into_iter()
        .filter_map(|v| {
            v.get("eid")
                .and_then(|s| s.as_str())
                .map(|s| EntityId::from(s.to_string()))
        })
        .collect())
}

/// Resolve FTS alias match and warn if multiple canonical entities are found.
///
/// Returns deduplicated entity IDs.  Logs WARN when >1 canonical (which can
/// only happen if the UNIQUE index is relaxed in a future migration).
pub async fn resolve_fts_warn_multi(
    db: &SurrealClient,
    value: &str,
    limit: usize,
) -> Vec<EntityId> {
    match resolve_fts(db, value, limit).await {
        Ok(ids) => {
            if ids.len() > 1 {
                warn!(
                    alias = value,
                    count = ids.len(),
                    "alias FTS resolved to multiple canonical entities"
                );
            } else if !ids.is_empty() {
                info!(alias = value, "alias FTS resolved entity");
            }
            ids
        }
        Err(e) => {
            warn!(alias = value, "alias FTS resolution failed: {e}");
            vec![]
        }
    }
}
