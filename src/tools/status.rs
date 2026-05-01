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

#[tracing::instrument(
    name = "kb_status",
    skip_all,
    fields(
        project = input.project.as_deref().unwrap_or(""),
        has_domain_filter = input.domain.is_some(),
    )
)]
pub async fn execute(db: &SurrealClient, input: StatusInput) -> Result<StatusReport> {
    search::status_query(db, input.project.as_deref(), input.domain.as_deref()).await
}
