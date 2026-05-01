//! Ledger for auto-consolidation cluster attempts.
//!
//! Tracks every cluster the auto-consolidation loop has evaluated so that:
//! - the same cluster is not re-evaluated during its cooldown window, and
//! - crash-recovery can inspect prior attempt state on daemon restart.
//!
//! Dedup key: `cluster_hash` (SHA-256 of sorted source IDs joined by '\n').
//! The hash is stable across restarts and independent of ID ordering.

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest as _, Sha256};
use surrealdb::types::RecordId;
use tracing::instrument;

use crate::db::client::SurrealClient;
use crate::error::{KbError, Result};
use crate::types::MemoryId;

// ── Outcome enum ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum AttemptOutcome {
    Merged,
    DeferredLowConfidence,
    DeferredOversized,
    DeferredSensitive,
    SidecarError,
    MergeFailed,
}

impl AttemptOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::DeferredLowConfidence => "deferred_low_confidence",
            Self::DeferredOversized => "deferred_oversized",
            Self::DeferredSensitive => "deferred_sensitive",
            Self::SidecarError => "sidecar_error",
            Self::MergeFailed => "merge_failed",
        }
    }
}

// ── Public functions ─────────────────────────────────────────────────────────

/// Stable hash of a cluster — sort source IDs, join with `\n`, SHA-256 hex.
///
/// Order-independent: hash([a,b,c]) == hash([c,b,a]).
pub fn cluster_hash(source_ids: &[MemoryId]) -> String {
    let mut sorted: Vec<&str> = source_ids.iter().map(|id| id.as_str()).collect();
    sorted.sort_unstable();
    let payload = sorted.join("\n");
    let digest = Sha256::digest(payload.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect::<String>()
}

/// Check if a cluster is currently in cooldown.
///
/// Returns `Some(skip_until)` when the cluster has a recorded attempt whose
/// `skip_until` is in the future; `None` when free to evaluate.
#[instrument(skip(db), fields(cluster_hash))]
pub async fn check_cooldown(
    db: &SurrealClient,
    cluster_hash: &str,
) -> Result<Option<DateTime<Utc>>> {
    let mut result = db
        .query(
            "SELECT skip_until FROM consolidation_attempt \
             WHERE cluster_hash = $hash LIMIT 1",
        )
        .bind(("hash", cluster_hash.to_string()))
        .await
        .map_err(|e| KbError::db("consolidation_ledger: check_cooldown", e))?;

    let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };

    let skip_until = parse_datetime(&row, "skip_until")?;
    let Some(t) = skip_until else {
        return Ok(None);
    };

    if t > Utc::now() {
        Ok(Some(t))
    } else {
        Ok(None)
    }
}

/// Record or update an attempt for the given cluster.
///
/// UPSERT semantics keyed on `cluster_hash`:
/// - If no row exists: INSERT with `attempt_count = 1`.
/// - If a row exists: UPDATE all mutable fields, increment `attempt_count`.
///
/// `cooldown_days` sets `skip_until = now + cooldown_days`.
#[instrument(skip(db, source_ids, reason, merged_into), fields(cluster_hash, outcome = outcome.as_str()))]
pub async fn record_attempt(
    db: &SurrealClient,
    cluster_hash: &str,
    source_ids: &[MemoryId],
    outcome: AttemptOutcome,
    reason: Option<String>,
    merged_into: Option<MemoryId>,
    cooldown_days: u32,
) -> Result<()> {
    let now = Utc::now();
    let skip_until = now + Duration::days(i64::from(cooldown_days));
    let outcome_str = outcome.as_str().to_string();

    let source_record_ids: Vec<RecordId> = source_ids
        .iter()
        .map(|id| RecordId::new("memory", id.as_str()))
        .collect();

    let merged_record_id: Option<RecordId> = merged_into
        .as_ref()
        .map(|id| RecordId::new("memory", id.as_str()));

    // IF EXISTS: update + increment; ELSE: insert with count=1.
    // SurrealDB v3 UPSERT requires a record-id subject — we use IF EXISTS / ELSE.
    db.query(
        "IF (SELECT 1 FROM consolidation_attempt WHERE cluster_hash = $hash LIMIT 1)[0] != NONE {
             UPDATE consolidation_attempt
             SET attempted_at  = $now,
                 last_outcome  = $outcome,
                 last_reason   = $reason,
                 merged_into   = $merged_into,
                 skip_until    = $skip_until,
                 attempt_count += 1
             WHERE cluster_hash = $hash
         } ELSE {
             CREATE consolidation_attempt SET
                 cluster_hash  = $hash,
                 source_ids    = $source_ids,
                 attempted_at  = $now,
                 last_outcome  = $outcome,
                 last_reason   = $reason,
                 merged_into   = $merged_into,
                 skip_until    = $skip_until,
                 attempt_count = 1
         }",
    )
    .bind(("hash", cluster_hash.to_string()))
    .bind(("source_ids", source_record_ids))
    .bind(("now", now))
    .bind(("outcome", outcome_str))
    .bind(("reason", reason))
    .bind(("merged_into", merged_record_id))
    .bind(("skip_until", skip_until))
    .await
    .map_err(|e| KbError::db("consolidation_ledger: record_attempt", e))?;

    Ok(())
}

/// Count consolidation attempts recorded in the last 24 hours (rolling window).
///
/// Counts all rows in `consolidation_attempt` where `attempted_at` is within
/// the last 24 hours, regardless of outcome. Used to enforce `max_attempts_per_day`.
pub async fn count_attempts_last_24h(db: &SurrealClient) -> Result<u32> {
    let since = Utc::now() - Duration::hours(24);
    let mut result = db
        .query(
            "SELECT count() AS n FROM consolidation_attempt \
             WHERE attempted_at >= $since GROUP ALL",
        )
        .bind(("since", since))
        .await
        .map_err(|e| KbError::db("consolidation_ledger: count_attempts_last_24h", e))?;

    let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();
    let count = rows
        .into_iter()
        .next()
        .and_then(|row| row.get("n").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    Ok(count as u32)
}

// ── Internal helpers ─────────────────────────────────────────────────────────

fn parse_datetime(
    row: &serde_json::Value,
    field: &str,
) -> Result<Option<DateTime<Utc>>> {
    let Some(v) = row.get(field) else {
        return Ok(None);
    };
    if v.is_null() {
        return Ok(None);
    }
    // SurrealDB may return datetime as a string or as { "@dt": "..." }
    let s = match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(obj) => {
            if let Some(inner) = obj.get("@dt").and_then(|v| v.as_str()) {
                inner.to_string()
            } else {
                return Ok(None);
            }
        }
        _ => return Ok(None),
    };
    s.parse::<DateTime<Utc>>()
        .map(Some)
        .map_err(|_| KbError::DataCorruption(format!("consolidation_attempt.{field}: unparseable datetime")))
}
