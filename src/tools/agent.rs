use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{agent, client::SurrealClient};
use crate::db::agent::RegisterInput;
use crate::error::Result;
use crate::types::{AgentFull, AgentId, AgentKind, AgentSummary};

// ---------------------------------------------------------------------------
// kb_agent_register
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AgentRegisterInput {
    /// Unique agent name (max 128 chars)
    pub name: String,
    /// Agent kind: assistant, reviewer, summarizer, researcher, custom
    pub kind: AgentKind,
    /// Underlying model identifier (e.g. "claude-opus-4")
    pub model: Option<String>,
    /// Semver or freeform version string
    pub version: Option<String>,
    /// Parent agent name (resolved to AgentId)
    pub parent: Option<String>,
    /// Project this agent operates within
    pub project: Option<String>,
    /// Freeform agent configuration (never logged)
    pub config: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AgentRegisterOutput {
    pub id: AgentId,
    pub message: String,
}

#[tracing::instrument(
    name = "kb_agent_register",
    skip_all,
    fields(
        kind = %input.kind,
        has_model = input.model.is_some(),
        has_parent = input.parent.is_some(),
    )
)]
pub async fn execute_register(
    db: &SurrealClient,
    input: AgentRegisterInput,
) -> Result<AgentRegisterOutput> {
    let name = input.name.clone();
    let id = agent::register(
        db,
        RegisterInput {
            name,
            kind: input.kind,
            model: input.model,
            version: input.version,
            parent: input.parent,
            project: input.project,
            config: input.config,
        },
    )
    .await?;

    Ok(AgentRegisterOutput {
        message: format!("registered agent {}", input.name),
        id,
    })
}

// ---------------------------------------------------------------------------
// kb_agent_retire
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AgentRetireInput {
    /// Agent id (prefix agent_) or name
    pub id_or_name: String,
    /// If true, soft-archives all memories authored by this agent (never hard-deletes)
    pub purge_memories: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AgentRetireOutput {
    pub message: String,
}

#[tracing::instrument(
    name = "kb_agent_retire",
    skip_all,
    fields(purge_memories = input.purge_memories.unwrap_or(false))
)]
pub async fn execute_retire(
    db: &SurrealClient,
    input: AgentRetireInput,
) -> Result<AgentRetireOutput> {
    let id_or_name = input.id_or_name.clone();
    agent::retire(db, id_or_name, input.purge_memories.unwrap_or(false)).await?;

    Ok(AgentRetireOutput {
        message: format!("retired agent {}", input.id_or_name),
    })
}

// ---------------------------------------------------------------------------
// kb_agent_query (replaces kb_agent_list + kb_agent_get)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AgentQueryInput {
    /// Agent id (prefix agent_) or name. When provided, returns a single AgentFull record.
    /// When omitted, returns a list of AgentSummary records.
    pub id_or_name: Option<String>,
    /// Filter list results by project (ignored when id_or_name is set)
    pub project: Option<String>,
    /// Include archived agents in list results (default: false, ignored when id_or_name is set)
    pub include_archived: Option<bool>,
}

/// Output struct for kb_agent_query.
///
/// When `id_or_name` was provided: `single` contains the full agent record, `list` is empty.
/// When listing: `list` contains agent summaries, `single` is None.
#[derive(Debug, Serialize, JsonSchema)]
pub struct AgentQueryOutput {
    pub single: Option<AgentFull>,
    pub list: Vec<AgentSummary>,
}

#[tracing::instrument(
    name = "kb_agent_query",
    skip_all,
    fields(
        lookup = input.id_or_name.is_some(),
        has_project = input.project.is_some(),
        include_archived = input.include_archived.unwrap_or(false),
    )
)]
pub async fn execute_query(
    db: &SurrealClient,
    input: AgentQueryInput,
) -> Result<AgentQueryOutput> {
    if let Some(id_or_name) = input.id_or_name {
        let full = agent::get(db, id_or_name).await?;
        Ok(AgentQueryOutput { single: Some(full), list: vec![] })
    } else {
        let list = agent::list(db, input.project, input.include_archived.unwrap_or(false)).await?;
        Ok(AgentQueryOutput { single: None, list })
    }
}

// ---------------------------------------------------------------------------
// Internal helpers kept for test use (not exposed as MCP tools)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AgentListInput {
    /// Filter by project
    pub project: Option<String>,
    /// Include archived agents (default: false)
    pub include_archived: Option<bool>,
}

pub async fn execute_list(
    db: &SurrealClient,
    input: AgentListInput,
) -> Result<Vec<AgentSummary>> {
    agent::list(db, input.project, input.include_archived.unwrap_or(false)).await
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AgentGetInput {
    /// Agent id (prefix agent_) or name
    pub id_or_name: String,
}

pub async fn execute_get(
    db: &SurrealClient,
    input: AgentGetInput,
) -> Result<AgentFull> {
    agent::get(db, input.id_or_name).await
}
