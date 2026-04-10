use schemars::JsonSchema;
use serde::Serialize;

use crate::config::AlertConfig;
use crate::db::{alerts, client::SurrealClient};
use crate::error::Result;
use crate::types::*;

#[derive(Debug, Serialize, JsonSchema)]
pub struct AlertsOutput {
    pub alerts: Vec<Alert>,
    pub count: usize,
}

#[tracing::instrument(skip_all, fields(tool = "kb_alerts"))]
pub async fn execute(db: &SurrealClient, config: &AlertConfig) -> Result<AlertsOutput> {
    let alerts = alerts::check_all(db, config).await?;
    let count = alerts.len();
    Ok(AlertsOutput { alerts, count })
}
