use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{client::SurrealClient, embeddings::LocalEmbedder, search};
use crate::error::Result;
use crate::types::*;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecallInput {
    /// Natural language or keyword query
    pub query: String,
    /// Filter by memory type
    #[serde(rename = "type")]
    pub memory_type: Option<MemoryType>,
    /// Filter by project
    pub project: Option<String>,
    /// Filter by domain tag (memory must include this domain)
    pub domain: Option<String>,
    /// Filter by person involved
    pub person: Option<String>,
    /// Only memories after this date
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// Only memories before this date
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    /// Max results (default 10, max 25)
    pub limit: Option<u8>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RecallOutput {
    pub memories: Vec<MemorySummary>,
    pub count: usize,
}

#[tracing::instrument(skip_all, fields(tool = "kb_recall"))]
pub async fn execute(
    db: &SurrealClient,
    embedding_client: Option<Arc<LocalEmbedder>>,
    input: RecallInput,
) -> Result<RecallOutput> {
    let limit = input.limit.unwrap_or(10).min(25);

    // Embed the query for vector similarity search (best-effort).
    let query_vector = if let Some(ref client) = embedding_client {
        if !input.query.trim().is_empty() {
            match client.embed(&input.query).await {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("query embedding failed, using BM25 only: {e}");
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let memories = search::recall(
        db,
        &input.query,
        query_vector,
        input.memory_type,
        input.project.as_deref(),
        input.domain.as_deref(),
        input.person.as_deref(),
        input.since,
        input.until,
        limit,
    )
    .await?;

    let count = memories.len();
    Ok(RecallOutput { memories, count })
}
