use chrono::{Duration, Utc};
use tracing::instrument;

use crate::config::AlertConfig;
use crate::db::client::SurrealClient;
use crate::db::BITEMPORAL_ACTIVE;
use crate::error::{KbError, Result};
use crate::types::{Alert, AlertReference, AlertType, MemoryId};

/// Run all alert checks and return the combined list.
#[instrument(skip_all)]
pub async fn check_all(db: &SurrealClient, config: &AlertConfig) -> Result<Vec<Alert>> {
    let mut alerts = Vec::new();

    alerts.extend(check_overdue_actions(db, config.overdue_action_item_days).await?);
    alerts.extend(check_expiring_memories(db, config.expiring_memory_hours).await?);
    alerts.extend(check_stale_contacts(db, config.stale_contact_days).await?);
    alerts.extend(check_unsuperseded_updates(db).await?);
    alerts.extend(check_extraction_backlog(db, config.extraction_backlog_threshold).await?);

    tracing::info!(count = alerts.len(), "alert check complete");
    Ok(alerts)
}

/// Return alerts for prospective/action_item memories whose `event_at` is
/// older than `days` ago (i.e. the deadline has passed).
async fn check_overdue_actions(db: &SurrealClient, days: u64) -> Result<Vec<Alert>> {
    let cutoff = Utc::now() - Duration::days(days as i64);

    let mut result = db
        .query(format!(
            "SELECT * FROM memory
             WHERE {BITEMPORAL_ACTIVE}
               AND visibility = 'standard'
               AND memory_type = 'prospective'
               AND subtype = 'action_item'
               AND event_at < $cutoff"
        ))
        // Bind DateTime<Utc> directly — do NOT go through serde_json::to_value,
        // which would produce a string that SurrealDB v3 won't coerce for
        // datetime comparisons.
        .bind(("cutoff", cutoff))
        .await
        .map_err(|e| KbError::db("check_overdue_actions query", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing overdue action rows", e))?;

    let now = Utc::now();
    let mut alerts = Vec::with_capacity(rows.len());

    for row in rows {
        let id = extract_id(&row);
        let summary = row
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("(no summary)")
            .to_string();

        // Summary is safe to include — WHERE clause gates on visibility = 'standard'
        // before any row reaches this point (Wave C visibility fix).
        alerts.push(Alert {
            alert_type: AlertType::OverdueActionItem,
            message: format!("Action item overdue: {summary}"),
            reference: AlertReference::Memory(MemoryId::from(id)),
            created_at: now,
        });
    }

    Ok(alerts)
}

/// Return alerts for active memories whose `expires_at` falls within the next
/// `hours` hours (so the user has a chance to promote them before they expire).
async fn check_expiring_memories(db: &SurrealClient, hours: u64) -> Result<Vec<Alert>> {
    let now = Utc::now();
    let horizon = now + Duration::hours(hours as i64);

    let mut result = db
        .query(format!(
            "SELECT * FROM memory
             WHERE {BITEMPORAL_ACTIVE}
               AND visibility = 'standard'
               AND expires_at != NONE
               AND expires_at <= $horizon
               AND expires_at > $now"
        ))
        .bind(("horizon", horizon))
        .bind(("now", now))
        .await
        .map_err(|e| KbError::db("check_expiring_memories query", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing expiring memory rows", e))?;

    let mut alerts = Vec::with_capacity(rows.len());

    for row in rows {
        let id = extract_id(&row);
        let summary = row
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("(no summary)")
            .to_string();

        // Summary is safe to include — WHERE clause gates on visibility = 'standard'
        // before any row reaches this point (Wave C visibility fix).
        alerts.push(Alert {
            alert_type: AlertType::ExpiringMemory,
            message: format!("Memory expiring soon: {summary}"),
            reference: AlertReference::Memory(MemoryId::from(id)),
            created_at: now,
        });
    }

    Ok(alerts)
}

/// Check for entity contacts that haven't been seen recently.
///
/// Queries entities with `attrs.last_contact` that is older than the
/// configured threshold. Since `attrs` is a FLEXIBLE object, `last_contact`
/// may be stored as a string or datetime — we use `type::datetime()` to
/// coerce for comparison, and skip entities where conversion fails.
async fn check_stale_contacts(db: &SurrealClient, days: u64) -> Result<Vec<Alert>> {
    let cutoff = Utc::now() - Duration::days(days as i64);

    let mut result = db
        .query(
            "SELECT name, kind, attrs.last_contact AS last_contact FROM entity
             WHERE attrs.last_contact != NONE
               AND kind = 'person'",
        )
        .await
        .map_err(|e| KbError::db("check_stale_contacts query", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing stale contact rows", e))?;

    let now = Utc::now();
    let mut alerts = Vec::new();

    for row in rows {
        let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("(unknown)");

        // last_contact is stored as an RFC-3339 string in attrs (FLEXIBLE object).
        // Parse it here; skip rows where the value is absent or unparseable.
        let last_contact_opt: Option<chrono::DateTime<Utc>> = row
            .get("last_contact")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok());

        if let Some(last_contact) = last_contact_opt
            && last_contact < cutoff
        {
            alerts.push(Alert {
                alert_type: AlertType::StaleContact,
                message: format!(
                    "No contact with {name} since {}",
                    last_contact.format("%Y-%m-%d")
                ),
                reference: AlertReference::EntityName(name.to_string()),
                created_at: now,
            });
        }
    }

    Ok(alerts)
}

/// Detect potential contradictions: memories linked via `updates` edge where
/// the old memory is still `active` (should have been superseded).
///
/// This catches cases where `kb_remember(updates_id=...)` created the edge but
/// the `supersede()` call failed or was skipped, leaving two conflicting
/// memories both active.
async fn check_unsuperseded_updates(db: &SurrealClient) -> Result<Vec<Alert>> {
    let mut result = db
        .query(
            "SELECT
                record::id(in) AS new_id,
                record::id(out) AS old_id,
                in.summary AS new_summary,
                out.summary AS old_summary
             FROM updates
             WHERE out.invalidated_at IS NONE
               AND out.visibility = 'standard'
               AND in.visibility = 'standard'
             LIMIT 10",
        )
        .await
        .map_err(|e| KbError::db("unsuperseded updates check", e))?;

    let rows: Vec<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing unsuperseded rows", e))?;

    let now = Utc::now();
    let mut alerts = Vec::with_capacity(rows.len());

    for row in rows {
        let old_id = row
            .get("old_id")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let old_summary = row
            .get("old_summary")
            .and_then(|v| v.as_str())
            .unwrap_or("(no summary)")
            .to_string();
        let new_summary = row
            .get("new_summary")
            .and_then(|v| v.as_str())
            .unwrap_or("(no summary)")
            .to_string();

        // Both summaries are safe to include — WHERE clause gates on
        // out.visibility = 'standard' AND in.visibility = 'standard'
        // before any row reaches this point (Wave C visibility fix).
        alerts.push(Alert {
            alert_type: AlertType::Contradiction,
            message: format!(
                "Memory still active despite being updated: \"{old_summary}\" superseded by \"{new_summary}\""
            ),
            reference: AlertReference::Memory(MemoryId::from(old_id)),
            created_at: now,
        });
    }

    Ok(alerts)
}

/// Return an ExtractionBacklog alert when the count of memories with a failed
/// extraction status exceeds the configured threshold.
///
/// Retryable statuses: anchor_failed, relation_failed, both_failed, sidecar_unavailable.
/// `pending` and `complete` and `skipped` are NOT included — only genuine failures.
async fn check_extraction_backlog(db: &SurrealClient, threshold: u64) -> Result<Vec<Alert>> {
    let mut result = db
        .query(format!(
            "SELECT count() AS total FROM memory
             WHERE {BITEMPORAL_ACTIVE}
               AND extraction_status IN ['anchor_failed', 'relation_failed', 'both_failed', 'sidecar_unavailable']
             GROUP ALL"
        ))
        .await
        .map_err(|e| KbError::db("check_extraction_backlog query", e))?;

    let row: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing extraction backlog count", e))?;

    let count = row
        .and_then(|v| v.get("total").and_then(|t| t.as_u64()))
        .unwrap_or(0);

    if count > threshold {
        let now = Utc::now();
        return Ok(vec![Alert {
            alert_type: AlertType::ExtractionBacklog,
            message: format!(
                "{count} memories have failed extraction status (threshold: {threshold}). \
                 Run --retry-failed-extraction to clear."
            ),
            reference: AlertReference::Count(count.to_string()),
            created_at: now,
        }]);
    }

    Ok(vec![])
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the bare memory ID string from a SurrealDB result row.
fn extract_id(row: &serde_json::Value) -> String {
    row.get("id")
        .and_then(|v| crate::db::helpers::record_id_bare(v, "memory"))
        .unwrap_or_default()
}
