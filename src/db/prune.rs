use chrono::Utc;
use tracing::{info, instrument};

use crate::config::PruningConfig;
use crate::db::client::SurrealClient;
use crate::db::{BITEMPORAL_ACTIVE, BITEMPORAL_ACTIVE_NO_TTL};
use crate::error::{KbError, Result};
use crate::types::MemorySubtype;

// ── Importance Immunity ────────────────────────────────────────────────────

/// Importance threshold for immunity — memories at or above this never decay.
const IMPORTANCE_IMMUNE_THRESHOLD: u8 = 5;

// ── Domain-Aware Decay Rates ───────────────────────────────────────────────

/// Returns a decay rate multiplier for a given domain.
/// Lower multiplier = slower decay.
/// Used by drain session for domain-aware scheduling.
#[allow(dead_code)]
pub fn domain_decay_rate(domains: &[&str]) -> f64 {
    // Check domains in priority order (lowest rate wins)
    for d in domains {
        let lower = d.to_lowercase();
        if lower == "philosophy" || lower == "personal" {
            return 0.25;
        }
        if lower == "health" || lower == "fitness" {
            return 0.5;
        }
    }
    1.0 // standard rate
}

/// Run all pruning strategies.
#[instrument(skip_all, fields(archive_threshold = config.importance_archive_threshold))]
pub async fn run_all(db: &SurrealClient, config: &PruningConfig) -> Result<()> {
    let expired = expire_ttl(db).await?;
    let decayed = sm2_decay(db, config).await?;
    let archived = archive_below_threshold(db, config).await?;
    info!(expired, decayed, archived, "pruning complete");
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

/// SM-2 exponential decay with importance immunity and domain-aware rates.
///
/// The SM-2 schedule works as follows:
/// - Each memory has `ease_factor` (default 2.5), `review_interval` (days),
///   and `next_review_at` (when the next recall is expected).
/// - When a memory is recalled, `search::recall()` updates its SM-2 fields:
///   interval grows exponentially (`interval *= ease_factor`), ease increases
///   slightly (+0.1, capped at 4.0).
/// - Here in the decay job, we check for memories that are PAST their
///   `next_review_at` without being accessed (access_count still 0 since last
///   reset). For these "overdue" memories:
///   1. Reduce importance by 1
///   2. Decrease ease_factor by 0.2 (min 1.3) — harder to remember = faster decay
///   3. Reset review_interval to 1 day — start the schedule fresh
///   4. Reset access_count to 0
///
/// Immunity rules (from ADR-008):
/// - importance >= 5 → immune to all decay
/// - subtype in [decision, value, life_goal] → immune
///
/// Domain-aware rates:
/// - health/fitness → 0.5x decay rate (infrequent access is normal)
/// - philosophy/personal → 0.25x rate (long-term by nature)
/// - working/* → TTL handles expiry, SM-2 skipped
///
/// Memories that DON'T have SM-2 fields yet (pre-migration-004 records) fall
/// back to the legacy interval-based decay.
#[instrument(skip_all)]
pub async fn sm2_decay(db: &SurrealClient, config: &PruningConfig) -> Result<u64> {
    let threshold = config.importance_archive_threshold as i64;
    let immune_threshold = IMPORTANCE_IMMUNE_THRESHOLD as i64;
    let immune_subtypes = MemorySubtype::immune_strs();

    // SM-2 aware decay: memories past their review date that haven't been accessed.
    // Excludes immune memories (importance >= 5 or protected subtypes).
    // Excludes working/* (TTL handles those).
    // Use time::now() inline rather than binding a Rust DateTime to avoid
    // potential format mismatches in datetime comparison.
    let mut sm2_result = db
        .query(format!(
            "UPDATE memory SET
                importance = importance - 1,
                ease_factor = IF (ease_factor ?? 2.5) - 0.2 > 1.3 {{ (ease_factor ?? 2.5) - 0.2 }} ELSE {{ 1.3 }},
                review_interval = 1,
                next_review_at = time::now() + 1d,
                access_count = 0,
                updated_at = time::now()
             WHERE {BITEMPORAL_ACTIVE}
               AND next_review_at != NONE
               AND next_review_at < time::now()
               AND access_count = 0
               AND importance > $threshold
               AND importance < $immune_threshold
               AND memory_type != 'working'
               AND subtype NOT IN $immune_subtypes
             RETURN AFTER"
        ))
        .bind(("threshold", threshold))
        .bind(("immune_threshold", immune_threshold))
        .bind(("immune_subtypes", immune_subtypes.clone()))
        .await
        .map_err(|e| KbError::db("SM-2 decay", e))?;

    let sm2_rows: Vec<serde_json::Value> = sm2_result.take(0)
        .map_err(|e| KbError::db("SM-2 decay take results", e))?;
    let sm2_count = sm2_rows.len() as u64;

    // Legacy fallback: memories without SM-2 fields use interval-based decay.
    // Same immunity rules apply.
    let mut legacy_result = db
        .query(format!(
            "UPDATE memory SET
                importance = importance - 1,
                updated_at = time::now()
             WHERE {BITEMPORAL_ACTIVE}
               AND next_review_at = NONE
               AND access_count = 0
               AND importance > $threshold
               AND importance < $immune_threshold
               AND memory_type != 'working'
               AND subtype NOT IN $immune_subtypes
               AND updated_at < time::now() - type::duration($interval)
             RETURN AFTER"
        ))
        .bind(("threshold", threshold))
        .bind(("immune_threshold", immune_threshold))
        .bind(("immune_subtypes", immune_subtypes))
        .bind(("interval", format!("{}d", config.access_decay_interval_days)))
        .await
        .map_err(|e| KbError::db("legacy access decay", e))?;

    let legacy_rows: Vec<serde_json::Value> = legacy_result.take(0).unwrap_or_default();
    let legacy_count = legacy_rows.len() as u64;

    let total = sm2_count + legacy_count;
    if total > 0 {
        info!(sm2 = sm2_count, legacy = legacy_count, "decayed memory importance");
    }
    Ok(total)
}

/// Archive any active memories whose importance has fallen to or below the
/// configured threshold. Respects the same immunity rules as sm2_decay —
/// protected subtypes and high-importance memories are never archived.
#[instrument(skip_all)]
async fn archive_below_threshold(db: &SurrealClient, config: &PruningConfig) -> Result<u64> {
    let threshold = config.importance_archive_threshold as i64;
    let immune_threshold = IMPORTANCE_IMMUNE_THRESHOLD as i64;
    let immune_subtypes = MemorySubtype::immune_strs();

    let mut result = db
        .query(format!(
            "UPDATE memory
             SET status = 'archived',
                 invalidated_at = time::now(),
                 invalidation_reason = 'expired',
                 updated_at = time::now()
             WHERE {BITEMPORAL_ACTIVE}
               AND importance < $threshold
               AND importance < $immune_threshold
               AND subtype NOT IN $immune_subtypes
             RETURN AFTER"
        ))
        .bind(("threshold", threshold))
        .bind(("immune_threshold", immune_threshold))
        .bind(("immune_subtypes", immune_subtypes))
        .await
        .map_err(|e| KbError::db("archive below threshold", e))?;

    let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();
    let count = rows.len() as u64;
    if count > 0 {
        info!(count, "archived memories below importance threshold");
    }
    Ok(count)
}
