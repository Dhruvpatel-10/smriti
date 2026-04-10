use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{client::SurrealClient, entity};
use crate::error::Result;
use crate::types::*;

// ---------------------------------------------------------------------------
// kb_entity_upsert
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EntityUpsertInput {
    /// Entity name (unique key)
    pub name: String,
    /// Entity kind: person, company, product, concept, project
    pub kind: String,
    /// Roles: investor, advisor, customer, etc.
    pub roles: Option<Vec<String>>,
    /// Freeform key-value attributes
    pub attrs: Option<serde_json::Value>,
    /// One-line context summary
    pub context: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EntityUpsertOutput {
    pub id: String,
    pub message: String,
}

#[tracing::instrument(skip_all, fields(tool = "kb_entity_upsert"))]
pub async fn execute_upsert(
    db: &SurrealClient,
    input: EntityUpsertInput,
) -> Result<EntityUpsertOutput> {
    let roles = input.roles.unwrap_or_default();
    let attrs = input.attrs.unwrap_or(serde_json::json!({}));

    let id = entity::upsert(db, &input.name, &input.kind, roles, attrs, input.context).await?;

    Ok(EntityUpsertOutput {
        id: id.to_string(),
        message: format!("Entity '{}' upserted", input.name),
    })
}

// ---------------------------------------------------------------------------
// kb_entity_find
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EntityFindInput {
    /// Name to search for (fuzzy match)
    pub query: String,
    /// Filter by kind (person, company, etc.)
    pub kind: Option<String>,
    /// Filter by role (investor, advisor, etc.)
    pub role: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EntityFindOutput {
    pub entities: Vec<EntitySummary>,
    pub count: usize,
}

#[tracing::instrument(skip_all, fields(tool = "kb_entity_find"))]
pub async fn execute_find(
    db: &SurrealClient,
    input: EntityFindInput,
) -> Result<EntityFindOutput> {
    let entities = entity::find(
        db,
        &input.query,
        input.kind.as_deref(),
        input.role.as_deref(),
    )
    .await?;

    let count = entities.len();
    Ok(EntityFindOutput { entities, count })
}

// ---------------------------------------------------------------------------
// kb_entity_get
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EntityGetInput {
    /// Entity ID to fetch
    pub id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EntityGetOutput {
    pub entity: EntityFull,
}

#[tracing::instrument(skip_all, fields(tool = "kb_entity_get"))]
pub async fn execute_get(
    db: &SurrealClient,
    input: EntityGetInput,
) -> Result<EntityGetOutput> {
    let id = EntityId::from(input.id);
    let entity = entity::get_full(db, &id).await?;
    Ok(EntityGetOutput { entity })
}
