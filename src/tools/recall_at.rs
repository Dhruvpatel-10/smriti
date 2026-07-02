use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{FusionConfig, MmrConfig};
use crate::db::{agent, client::SurrealClient, embeddings::Embedder, reranker::Reranker, search};
use crate::db::search::AuthorFilter;
use crate::error::{KbError, Result};
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
    /// Max results (default 10, max 50)
    pub limit: Option<u8>,
    /// Author filter: "me" = user only (author IS NONE); "<agent_name>" = that agent only; omitted = no filter
    pub author_filter: Option<String>,
    /// Filter by channel (cli, slack, discord, email, sms, web, api, github, voice_app, notion, calendar)
    pub channel: Option<Channel>,
    /// Filter by modality (text, transcript, document, code, structured, image)
    pub modality: Option<Modality>,
    /// When true (default), exclude memories explicitly retracted as wrong by the user.
    /// Set to false only for forensic/audit work where seeing disowned facts is intentional.
    pub exclude_retracted: Option<bool>,
    /// Reserved for forward-compat with ADR-029. kb_recall_at has no SA stage today;
    /// this field is accepted and ignored so callers can use a uniform parameter set.
    #[serde(default)]
    pub spreading: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RecallAtOutput {
    pub memories: Vec<MemorySummary>,
    pub count: usize,
}

#[tracing::instrument(
    name = "kb_recall_at",
    skip_all,
    fields(
        as_of = %input.as_of,
        project = input.project.as_deref().unwrap_or(""),
        limit = input.limit,
        query_len = input.query.len(),
        has_author_filter = input.author_filter.is_some(),
    )
)]
pub async fn execute(
    db: &SurrealClient,
    embedding_client: Option<Arc<dyn Embedder>>,
    reranker: Option<Arc<Reranker>>,
    input: RecallAtInput,
    fusion_cfg: &FusionConfig,
    mmr_cfg: &MmrConfig,
) -> Result<RecallAtOutput> {
    let limit = input.limit.unwrap_or(10).min(50);

    // Resolve author_filter to the typed AuthorFilter enum.
    let resolved_author = match input.author_filter.as_deref() {
        None => AuthorFilter::None,
        Some("me") => AuthorFilter::UserOnly,
        Some(name) => {
            let ag = agent::resolve_by_name(db, name).await?.ok_or_else(|| {
                KbError::InvalidInput(format!("unknown agent: {name}"))
            })?;
            // Archived agents are allowed for historical queries.
            AuthorFilter::Agent(ag.id)
        }
    };

    // Embed the query for vector similarity search (best-effort).
    // Same asymmetric query path as kb_recall; soft-fail to BM25-only per ADR-005.
    let query_vector = if let Some(ref client) = embedding_client {
        if !input.query.trim().is_empty() {
            match client.embed_query(&input.query).await {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("recall_at query embedding failed, using BM25 only: {e}");
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let exclude_retracted = input.exclude_retracted.unwrap_or(true);

    let memories = search::recall_at(
        db,
        &input.query,
        query_vector,
        reranker.as_deref(),
        input.as_of,
        input.project.as_deref(),
        input.domain.as_deref(),
        input.channel,
        input.modality,
        limit,
        &resolved_author,
        fusion_cfg,
        exclude_retracted,
        mmr_cfg,
    )
    .await?;

    let count = memories.len();
    Ok(RecallAtOutput { memories, count })
}
