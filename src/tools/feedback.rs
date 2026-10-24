//! kb_feedback + recall_event logging (B3 feedback loop).
//!
//! Every kb_recall appends a `recall_event` (ids only, never text) and returns
//! its `trace_id`. kb_feedback reports what happened to those results; it is
//! the only input that adapts ranking. Feedback never archives or rewrites a
//! memory: `used`/`irrelevant` move a decayed `salience` prior, `wrong` tags
//! the memory `disputed`, `pin`/`unpin` toggle `is_static`.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::db::client::SurrealClient;
use crate::db::memory;
use crate::error::{KbError, Result};
use crate::types::*;

/// Half-life of the salience prior, in days.
const SALIENCE_HALF_LIFE_DAYS: f64 = 30.0;
/// Most ids accepted in one feedback call.
const MAX_IDS: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackSignal {
    /// The memory was useful for the task.
    Used,
    /// Returned but not relevant to the query.
    Irrelevant,
    /// The memory is factually wrong (tagged `disputed`; never archived).
    Wrong,
    /// Always keep: exempt from TTL expiry.
    Pin,
    /// Undo pin.
    Unpin,
}

impl FeedbackSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Used => "used",
            Self::Irrelevant => "irrelevant",
            Self::Wrong => "wrong",
            Self::Pin => "pin",
            Self::Unpin => "unpin",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FeedbackInput {
    /// trace_id returned by kb_recall. When given, every id must be one of that recall's results.
    pub trace_id: Option<String>,
    /// Memory IDs the signal applies to (1–50).
    pub memory_ids: Vec<String>,
    pub signal: FeedbackSignal,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FeedbackOutput {
    pub message: String,
    pub applied: usize,
}

/// Append a recall_event for these results and return its trace_id.
/// Logging failure never fails the recall.
pub async fn record_recall(db: &SurrealClient, memories: &[MemorySummary]) -> String {
    let trace_id = format!("trc_{}", uuid::Uuid::now_v7().as_simple());
    let ids: Vec<String> = memories.iter().map(|m| m.id.as_str().to_string()).collect();
    let tokens: i64 = memories.iter().map(|m| (m.summary.len() / 4) as i64).sum();
    if let Err(e) = db
        .query("CREATE recall_event CONTENT { trace_id: $t, memory_ids: $ids, tokens_returned: $tok }")
        .bind(("t", trace_id.clone()))
        .bind(("ids", ids))
        .bind(("tok", tokens))
        .await
        .and_then(|r| r.check())
    {
        warn!(trace_id = %trace_id, "recall_event write failed: {e}");
    }
    trace_id
}

#[tracing::instrument(name = "kb_feedback", skip_all, fields(signal = input.signal.as_str(), n = input.memory_ids.len()))]
pub async fn execute(db: &SurrealClient, input: FeedbackInput) -> Result<FeedbackOutput> {
    if input.memory_ids.is_empty() || input.memory_ids.len() > MAX_IDS {
        return Err(KbError::InvalidInput(format!("memory_ids must hold 1–{MAX_IDS} ids")));
    }

    // A trace pins feedback to what that recall actually returned.
    if let Some(trace) = &input.trace_id {
        let rows: Vec<serde_json::Value> = db
            .query("SELECT memory_ids FROM recall_event WHERE trace_id = $t LIMIT 1")
            .bind(("t", trace.clone()))
            .await
            .and_then(|mut r| r.take(0))
            .map_err(|e| KbError::db("reading recall_event", e))?;
        let returned: HashSet<String> = rows
            .first()
            .and_then(|r| r.get("memory_ids"))
            .and_then(|v| v.as_array())
            .ok_or_else(|| KbError::InvalidInput(format!("unknown trace_id {trace}")))?
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        if let Some(stray) = input.memory_ids.iter().find(|id| !returned.contains(*id)) {
            return Err(KbError::InvalidInput(format!(
                "memory {stray} was not returned by trace {trace}"
            )));
        }
    }

    let mut applied = 0;
    for raw in &input.memory_ids {
        let id = MemoryId::from(raw.clone());
        memory::get(db, &id).await?; // NotFound for unknown ids
        apply(db, &id, input.signal).await?;
        db.query("CREATE feedback_event CONTENT { trace_id: $t, memory_id: $m, signal: $s }")
            .bind(("t", input.trace_id.clone()))
            .bind(("m", raw.clone()))
            .bind(("s", input.signal.as_str()))
            .await
            .and_then(|r| r.check())
            .map_err(|e| KbError::db("writing feedback_event", e))?;
        applied += 1;
    }

    Ok(FeedbackOutput {
        message: format!("{} feedback recorded for {applied} memories", input.signal.as_str()),
        applied,
    })
}

async fn apply(db: &SurrealClient, id: &MemoryId, signal: FeedbackSignal) -> Result<()> {
    let sql = match signal {
        FeedbackSignal::Used | FeedbackSignal::Irrelevant => {
            let delta = if signal == FeedbackSignal::Used { 1.0 } else { -0.5 };
            return bump_salience(db, id, delta).await;
        }
        FeedbackSignal::Wrong => {
            "UPDATE type::record('memory', $id) SET tags = array::union(tags ?? [], ['disputed'])"
        }
        FeedbackSignal::Pin => "UPDATE type::record('memory', $id) SET is_static = true, expires_at = NONE",
        FeedbackSignal::Unpin => "UPDATE type::record('memory', $id) SET is_static = false",
    };
    db.query(sql)
        .bind(("id", id.as_str().to_string()))
        .await
        .and_then(|r| r.check())
        .map(|_| ())
        .map_err(|e| KbError::db("applying feedback", e))
}

async fn bump_salience(db: &SurrealClient, id: &MemoryId, delta: f64) -> Result<()> {
    let rows: Vec<serde_json::Value> = db
        .query("SELECT salience ?? 0.0 AS s, salience_at FROM type::record('memory', $id)")
        .bind(("id", id.as_str().to_string()))
        .await
        .and_then(|mut r| r.take(0))
        .map_err(|e| KbError::db("reading salience", e))?;
    let row = rows.first();
    let prior = row.and_then(|r| r.get("s")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let at = row
        .and_then(|r| r.get("salience_at"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<DateTime<Utc>>().ok());
    db.query("UPDATE type::record('memory', $id) SET salience = $v, salience_at = time::now()")
        .bind(("id", id.as_str().to_string()))
        .bind(("v", decayed(prior, at, Utc::now()) + delta))
        .await
        .and_then(|r| r.check())
        .map(|_| ())
        .map_err(|e| KbError::db("updating salience", e))
}

/// Exponentially decay a salience value from `at` to `now`.
fn decayed(value: f64, at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> f64 {
    let Some(at) = at else { return value };
    let days = (now - at).num_seconds().max(0) as f64 / 86_400.0;
    value * 0.5f64.powf(days / SALIENCE_HALF_LIFE_DAYS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn salience_halves_every_half_life() {
        let now = Utc::now();
        assert!((decayed(4.0, Some(now - Duration::days(30)), now) - 2.0).abs() < 1e-6);
        assert_eq!(decayed(4.0, None, now), 4.0);
    }
}
