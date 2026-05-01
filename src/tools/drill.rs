use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{client::SurrealClient, memory};
use crate::error::Result;
use crate::types::*;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DrillInput {
    /// Memory ID to fetch full content for
    pub id: String,
    /// Project context — if provided, sensitive memories belonging to this project are visible
    pub project: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DrillOutput {
    pub memory: Memory,
}

#[tracing::instrument(
    name = "kb_drill",
    skip_all,
    fields(id = %input.id)
)]
pub async fn execute(db: &SurrealClient, input: DrillInput) -> Result<DrillOutput> {
    let id = MemoryId::from(input.id);
    let mem = memory::get_for_drill(db, &id, input.project.as_deref())
        .await?
        .ok_or_else(|| crate::error::KbError::NotFound(id.to_string()))?;
    Ok(DrillOutput { memory: mem })
}
