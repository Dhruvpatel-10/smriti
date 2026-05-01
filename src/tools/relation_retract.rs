use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{client::SurrealClient, typed_relation};
use crate::error::{KbError, Result};
use crate::types::{MemoryId, RelationKind};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RelationRetractInput {
    /// From-memory ID (record key without the "memory:" prefix)
    pub from: String,
    /// To-memory ID (record key without the "memory:" prefix)
    pub to: String,
    /// Kind of the typed_relation (snake_case, e.g. "causes")
    pub kind: String,
    /// Optional human-readable reason for retraction
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RelationRetractOutput {
    pub message: String,
    pub retracted: bool,
}

#[tracing::instrument(
    name = "kb_relation_retract",
    skip_all,
    fields(from = %input.from, to = %input.to, kind = %input.kind)
)]
pub async fn execute(
    db: &SurrealClient,
    input: RelationRetractInput,
) -> Result<RelationRetractOutput> {
    if input.from.trim().is_empty() {
        return Err(KbError::InvalidInput("from must not be empty".into()));
    }
    if input.to.trim().is_empty() {
        return Err(KbError::InvalidInput("to must not be empty".into()));
    }

    let kind = input
        .kind
        .parse::<RelationKind>()
        .map_err(KbError::InvalidInput)?;

    let from = MemoryId::from(input.from);
    let to = MemoryId::from(input.to);

    let retracted =
        typed_relation::retract(db, &from, &to, kind, input.reason.as_deref()).await?;

    let message = if retracted {
        format!("retracted typed_relation {from} -[{kind}]-> {to}")
    } else {
        format!("no active typed_relation {from} -[{kind}]-> {to} found (already retracted or missing)")
    };

    Ok(RelationRetractOutput { message, retracted })
}
