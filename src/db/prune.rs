use chrono::Utc;
use tracing::{info, instrument};

use crate::config::PruningConfig;
use crate::db::client::SurrealClient;
use crate::db::BITEMPORAL_ACTIVE_NO_TTL;
use crate::error::{KbError, Result};

/// Run all pruning strategies.
///
/// Only explicit expiry (`expires_at` from a TTL or the caller) archives anything.
/// Disuse never lowers importance or archives a memory: importance is the
/// caller's prior, and relevance-by-use belongs in a ranking signal, not in
/// destructive decay (MP/1 invariant I5). SM-2 decay and the importance-floor
/// archive were removed for that reason.
#[instrument(skip_all)]
pub async fn run_all(db: &SurrealClient, _config: &PruningConfig) -> Result<()> {
    let expired = expire_ttl(db).await?;
    info!(expired, "pruning complete");
    Ok(())
}

/// Archive memories past their `expires_at` (TTL-based expiry).
///
/// LOW: `RETURN AFTER` fetches full rows to obtain a count. SurrealDB v3
/// UPDATE does not expose a `RETURN count` projection, so splitting into a
/// SELECT count + UPDATE RETURN NONE would require two round-trips. On
/// SurrealKV (embedded, single-writer) the full-row serialization cost is
/// acceptable for the small result sets typical of TTL expiry. Revisit if
/// profiling shows this as a hot path.
#[instrument(skip(db))]
pub async fn expire_ttl(db: &SurrealClient) -> Result<u64> {
    let now = Utc::now();
    let mut result = db
        .query(format!(
            "UPDATE memory
             SET status = 'archived',
                 invalidated_at = time::now(),
                 invalidation_reason = 'expired',
                 updated_at = time::now()
             WHERE {BITEMPORAL_ACTIVE_NO_TTL}
               AND expires_at != NONE
               AND expires_at < $now
             RETURN AFTER"
        ))
        .bind(("now", now))
        .await
        .map_err(|e| KbError::db("TTL expiry", e))?;

    let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();
    let count = rows.len() as u64;
    if count > 0 {
        info!(count, "expired TTL memories");
    }
    Ok(count)
}
