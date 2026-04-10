use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{client::SurrealClient, graph, memory};
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

#[tracing::instrument(skip_all, fields(tool = "kb_consolidate"))]
pub async fn execute(db: &SurrealClient, input: ConsolidateInput) -> Result<ConsolidateOutput> {
    if input.ids.is_empty() {
        return Err(crate::error::KbError::InvalidInput(
            "must provide at least one memory ID to consolidate".to_string(),
        ));
    }

    // Read the first source memory to inherit metadata
    let first_id = MemoryId::from(input.ids[0].clone());
    let first = memory::get(db, &first_id).await?;

    // Create the consolidated memory
    let new_id = memory::create(
        db,
        input.result_summary,
        None,
        first.memory_type,
        first.subtype,
        first.source,
        first.project,
        first.domain.clone(),
        first.people.clone(),
        first.importance,
        first.visibility,
        Some(first.event_at),
        first.tags,
        None, // no TTL for consolidated memories
        first.source_agent.clone(),
        None, // session_id: new consolidated memory gets none
    )
    .await?;

    // Archive originals and link them
    for id_str in &input.ids {
        let old_id = MemoryId::from(id_str.clone());
        memory::archive(db, &old_id).await?;
        graph::link_memories(db, &new_id, &old_id, RelationType::Derives).await?;
    }

    Ok(ConsolidateOutput {
        id: new_id.to_string(),
        message: format!("Consolidated {} memories", input.ids.len()),
    })
}
