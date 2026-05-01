use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{client::SurrealClient, consolidation::{self, ConsolidateParams}, embeddings::Embedder, memory, validation};
use crate::error::Result;
use crate::types::*;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConsolidateInput {
    /// Memory IDs to merge
    pub ids: Vec<String>,
    /// Summary for the consolidated memory
    pub result_summary: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ConsolidateOutput {
    pub id: String,
    pub message: String,
}

#[tracing::instrument(
    name = "kb_consolidate",
    skip_all,
    fields(
        ids_count = input.ids.len(),
        result_summary_len = input.result_summary.len(),
    )
)]
pub async fn execute(
    db: &SurrealClient,
    embedding_client: Option<Arc<dyn Embedder>>,
    input: ConsolidateInput,
) -> Result<ConsolidateOutput> {
    if input.ids.is_empty() {
        return Err(crate::error::KbError::InvalidInput(
            "must provide at least one memory ID to consolidate".to_string(),
        ));
    }

    if input.ids.len() > 50 {
        return Err(crate::error::KbError::InvalidInput(
            "too many ids for consolidation, max 50".to_string(),
        ));
    }

    // Validate the summary before hitting the DB.
    validation::validate_summary(&input.result_summary)?;

    let source_ids: Vec<MemoryId> = input
        .ids
        .iter()
        .map(|s| MemoryId::from(s.clone()))
        .collect();

    // Read the first source to get source_agent for forwarding and to build
    // the embedding contextual text. Aggregated metadata (importance, people,
    // tags, domain, visibility) is now computed inside consolidate_atomic.
    let first_id = MemoryId::from(input.ids[0].clone());
    let first = memory::get(db, &first_id).await?;

    // Embed BEFORE the transaction — embedding is fallible and must not be
    // inside the DB write path (matches remember.rs pattern).
    // The contextual text uses first-source fields for type/subtype/project context;
    // this is acceptable since heuristic 1 clusters by subtype+project.
    let vector: Option<Vec<f32>> = if let Some(ref client) = embedding_client {
        let embed_text = validation::contextual_text(
            &input.result_summary,
            first.memory_type,
            first.subtype,
            first.project.as_deref(),
            &first.domain,
            &first.people,
            first.channel,
            first.modality,
        );
        // Consolidated memory is a stored document — use document input_type
        // (Voyage asymmetric path); local backend ignores the kind.
        match client.embed_document(&embed_text).await {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("embedding failed for consolidation (storing without vector): {e}");
                None
            }
        }
    } else {
        None
    };

    // Metadata aggregation (importance, people, tags, domain, visibility) is
    // handled inside consolidate_atomic via fetch_source_metadata.
    let params = ConsolidateParams {
        summary: input.result_summary,
        vector,
        source_agent: first.source_agent,
        event_at: None,
    };

    let new_id = consolidation::consolidate_atomic(db, &source_ids, params).await?;

    Ok(ConsolidateOutput {
        id: new_id.to_string(),
        message: format!("Consolidated {} memories", input.ids.len()),
    })
}
