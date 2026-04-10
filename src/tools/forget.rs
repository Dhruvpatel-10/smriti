use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{client::SurrealClient, memory};
use crate::error::Result;
use crate::types::*;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ForgetInput {
    /// Memory ID to archive
    pub id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ForgetOutput {
    pub message: String,
}

#[tracing::instrument(skip_all, fields(tool = "kb_forget"))]
pub async fn execute(db: &SurrealClient, input: ForgetInput) -> Result<ForgetOutput> {
    let id = MemoryId::from(input.id);
    memory::archive(db, &id).await?;
    Ok(ForgetOutput {
        message: format!("Memory {id} archived"),
    })
}
