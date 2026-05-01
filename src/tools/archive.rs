use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{client::SurrealClient, memory};
use crate::error::Result;
use crate::types::*;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ArchiveInput {
    /// Memory ID to archive
    pub id: String,
    /// Why this memory is being archived: forget (no longer relevant) or retract (explicitly wrong)
    pub reason: ArchiveReason,
    /// When reason=retract: optional ID of a correction memory that supersedes this one
    /// (creates a retracts audit edge). Ignored when reason=forget.
    pub correction_id: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ArchiveOutput {
    pub message: String,
}

#[tracing::instrument(
    name = "kb_archive",
    skip_all,
    fields(id = %input.id, reason = ?input.reason, has_correction = input.correction_id.is_some())
)]
pub async fn execute(db: &SurrealClient, input: ArchiveInput) -> Result<ArchiveOutput> {
    let id = MemoryId::from(input.id.clone());

    let message = match input.reason {
        ArchiveReason::Forget => {
            memory::archive(db, &id, InvalidationReason::Forgotten).await?;
            format!("Memory {id} archived")
        }
        ArchiveReason::Retract => {
            let correction_id = input.correction_id.map(MemoryId::from);
            memory::retract(db, &id, correction_id.as_ref()).await?;
            match &correction_id {
                Some(corr_id) => format!("retracted memory {id} with correction {corr_id}"),
                None => format!("retracted memory {id}"),
            }
        }
    };

    Ok(ArchiveOutput { message })
}
