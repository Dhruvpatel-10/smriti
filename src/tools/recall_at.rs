use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{client::SurrealClient, search};
use crate::error::Result;
use crate::types::*;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecallAtInput {
    /// Natural language or keyword query
    pub query: String,
    /// Return what was active AT this point in time
    pub as_of: chrono::DateTime<chrono::Utc>,
    /// Filter by project
    pub project: Option<String>,
    /// Filter by domain tag
    pub domain: Option<String>,
    /// Max results (default 10, max 25)
    pub limit: Option<u8>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RecallAtOutput {
    pub memories: Vec<MemorySummary>,
    pub count: usize,
}

#[tracing::instrument(skip_all, fields(tool = "kb_recall_at"))]
pub async fn execute(db: &SurrealClient, input: RecallAtInput) -> Result<RecallAtOutput> {
    let limit = input.limit.unwrap_or(10).min(25);

    let memories = search::recall_at(
        db,
        &input.query,
        input.as_of,
        input.project.as_deref(),
        input.domain.as_deref(),
        limit,
    )
    .await?;

    let count = memories.len();
    Ok(RecallAtOutput { memories, count })
}
