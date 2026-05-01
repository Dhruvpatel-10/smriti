use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{client::SurrealClient, entity};
use crate::error::{KbError, Result};
use crate::types::EntityId;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EntityDeleteInput {
    /// Entity ID to soft-delete
    pub id: String,
    /// Optional human-readable reason for archival
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EntityDeleteOutput {
    pub message: String,
    pub deleted: bool,
}

#[tracing::instrument(
    name = "kb_entity_delete",
    skip_all,
    fields(id = %input.id)
)]
pub async fn execute(db: &SurrealClient, input: EntityDeleteInput) -> Result<EntityDeleteOutput> {
    if input.id.trim().is_empty() {
        return Err(KbError::InvalidInput("id must not be empty".into()));
    }

    let id = EntityId::from(input.id);
    let deleted = entity::archive(db, &id, input.reason.as_deref()).await?;

    let message = if deleted {
        format!("entity {id} archived")
    } else {
        format!("entity {id} not found or already archived")
    };

    Ok(EntityDeleteOutput { message, deleted })
}
