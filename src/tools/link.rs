use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{client::SurrealClient, graph};
use crate::error::Result;
use crate::types::*;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LinkInput {
    /// Source memory ID
    pub from_id: String,
    /// Target memory ID
    pub to_id: String,
    /// Relation type
    pub relation: RelationType,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LinkOutput {
    pub message: String,
}

#[tracing::instrument(
    name = "kb_link",
    skip_all,
    fields(
        from_id = %input.from_id,
        to_id = %input.to_id,
        relation = ?input.relation,
    )
)]
pub async fn execute(db: &SurrealClient, input: LinkInput) -> Result<LinkOutput> {
    let from = MemoryId::from(input.from_id);
    let to = MemoryId::from(input.to_id);

    graph::link_memories(db, &from, &to, input.relation).await?;

    Ok(LinkOutput {
        message: format!("Linked {from} -[{}]-> {to}", input.relation),
    })
}
