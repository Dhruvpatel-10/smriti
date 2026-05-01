use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{client::SurrealClient, memory};
use crate::error::Result;
use crate::types::*;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PromoteInput {
    /// Memory ID to promote to long-term
    pub id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PromoteOutput {
    pub message: String,
}

#[tracing::instrument(
    name = "kb_promote",
    skip_all,
    fields(id = %input.id)
)]
pub async fn execute(db: &SurrealClient, input: PromoteInput) -> Result<PromoteOutput> {
    let id = MemoryId::from(input.id);
    memory::promote(db, &id).await?;
    Ok(PromoteOutput {
        message: format!("Memory {id} promoted to long-term"),
    })
}
