use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{client::SurrealClient, memory};
use crate::error::Result;
use crate::types::*;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DrillInput {
    /// Memory ID to fetch full content for
    pub id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DrillOutput {
    pub memory: MemoryFull,
}

#[tracing::instrument(skip_all, fields(tool = "kb_drill"))]
pub async fn execute(db: &SurrealClient, input: DrillInput) -> Result<DrillOutput> {
    let id = MemoryId::from(input.id);
    let mem = memory::get_and_touch(db, &id).await?;
    Ok(DrillOutput {
        memory: MemoryFull::from(mem),
    })
}
