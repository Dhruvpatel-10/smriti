use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{FusionConfig, SpreadingConfig};
use crate::db::{agent, client::SurrealClient, embeddings::Embedder, reranker::Reranker, search};
use crate::db::search::AuthorFilter;
use crate::error::{KbError, Result};
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
    /// Author filter: "me" = user only (author IS NONE); "<agent_name>" = that agent only; omitted = no filter
    pub author_filter: Option<String>,
    /// Minimum fused RRF score for a memory to appear in results. Higher =
    /// stricter precision (fewer, more confident results); lower = more recall.
    /// RRF score for a single-source rank-0 hit is ~0.0164; for an item ranked
    /// #1 in both BM25 and vector lists it is ~0.0327. Default 0.0 (no filter).
    pub min_score: Option<f32>,
    /// Filter by channel (cli, slack, discord, email, sms, web, api, github, voice_app, notion, calendar)
    pub channel: Option<Channel>,
    /// Filter by modality (text, transcript, document, code, structured, image)
    pub modality: Option<Modality>,
    /// Caller rerank override: true = force rerank, false = suppress rerank.
    /// Omit to use the configured policy (default: adaptive). ADR-022.
    #[serde(default)]
    pub rerank: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RecallOutput {
    pub memories: Vec<MemorySummary>,
    pub count: usize,
}

#[tracing::instrument(
    name = "kb_recall",
    skip_all,
    fields(
        memory_type = ?input.memory_type,
        project = input.project.as_deref().unwrap_or(""),
        has_person_filter = input.person.is_some(),
        has_since = input.since.is_some(),
        has_until = input.until.is_some(),
        limit = input.limit,
        query_len = input.query.len(),
        has_author_filter = input.author_filter.is_some(),
    )
)]
pub async fn execute(
    db: &SurrealClient,
    embedding_client: Option<Arc<dyn Embedder>>,
    reranker: Option<Arc<Reranker>>,
    input: RecallInput,
    spreading_cfg: &SpreadingConfig,
    fusion_cfg: &FusionConfig,
) -> Result<RecallOutput> {
    let limit = input.limit.unwrap_or(10).min(25);

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
    // Voyage backend uses asymmetric input_type=query for retrieval-tuned vectors;
    // local backend ignores the kind. Falling back to BM25-only on embed failure
    // honours ADR-005 — vector search is best-effort, network outages must not
    // hard-fail recall.
    let query_vector = if let Some(ref client) = embedding_client {
        if !input.query.trim().is_empty() {
            match client.embed_query(&input.query).await {
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

    let min_score = input.min_score.unwrap_or(0.0).max(0.0) as f64;
    let caller_override = input.rerank;
    let policy = reranker
        .as_deref()
        .map(|r| r.policy())
        .unwrap_or(RerankerPolicy::Adaptive);

    let memories = search::recall(
        db,
        &input.query,
        query_vector,
        reranker.as_deref(),
        input.memory_type,
        input.project.as_deref(),
        input.domain.as_deref(),
        input.person.as_deref(),
        input.since,
        input.until,
        input.channel,
        input.modality,
        limit,
        &resolved_author,
        min_score,
        spreading_cfg,
        fusion_cfg,
        policy,
        caller_override,
    )
    .await?;

    let count = memories.len();
    Ok(RecallOutput { memories, count })
}
