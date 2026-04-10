use schemars::JsonSchema;
use serde::Deserialize;

use crate::db::{client::SurrealClient, search};
use crate::error::Result;
use crate::types::*;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StatusInput {
    /// Filter by project (if None, returns across all projects)
    pub project: Option<String>,
    /// Filter by domain tag (only memories that include this domain)
    pub domain: Option<String>,
}

#[tracing::instrument(skip_all, fields(tool = "kb_status"))]
pub async fn execute(db: &SurrealClient, input: StatusInput) -> Result<StatusReport> {
    search::status_query(db, input.project.as_deref(), input.domain.as_deref()).await
}
