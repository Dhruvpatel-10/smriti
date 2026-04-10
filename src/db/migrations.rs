//! Schema migration system.
//!
//! Rules:
//! - NEVER edit an existing migration. Add a new one.
//! - Each migration must be idempotent (use IF NOT EXISTS / IF EXISTS).
//! - Migrations run in version order, skipping already-applied ones.
//! - The `_migration` table tracks what's been applied.

use chrono::Utc;
use tracing::{info, instrument};

use crate::db::client::SurrealClient;
use crate::error::{KbError, Result};

struct Migration {
    version: u32,
    description: &'static str,
    up: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        description: "initial schema: memory, entity, person, graph edge tables, FTS indexes",
        up: MIGRATION_001,
    },
    Migration {
        version: 2,
        description: "unique indexes on graph edge tables to prevent duplicate RELATE edges",
        up: MIGRATION_002,
    },
    Migration {
        version: 3,
        description: "vector embedding field and MTREE index for semantic search",
        up: MIGRATION_003,
    },
    Migration {
        version: 4,
        description: "SM-2 spaced repetition fields for exponential forgetting curve",
        up: MIGRATION_004,
    },
];

// ── Migration SQL ────────────────────────────────────────────────────────────

const MIGRATION_001: &str = r#"
    DEFINE TABLE IF NOT EXISTS memory SCHEMAFULL;
    DEFINE FIELD IF NOT EXISTS summary      ON TABLE memory TYPE string;
    DEFINE FIELD IF NOT EXISTS content      ON TABLE memory TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS memory_type  ON TABLE memory TYPE string;
    DEFINE FIELD IF NOT EXISTS subtype      ON TABLE memory TYPE string;
    DEFINE FIELD IF NOT EXISTS source       ON TABLE memory TYPE string;
    DEFINE FIELD IF NOT EXISTS source_ref   ON TABLE memory TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS project      ON TABLE memory TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS people       ON TABLE memory TYPE array;
    DEFINE FIELD IF NOT EXISTS people.*     ON TABLE memory TYPE string;
    DEFINE FIELD IF NOT EXISTS importance   ON TABLE memory TYPE int;
    DEFINE FIELD IF NOT EXISTS status       ON TABLE memory TYPE string DEFAULT 'active';
    DEFINE FIELD IF NOT EXISTS tags         ON TABLE memory TYPE array;
    DEFINE FIELD IF NOT EXISTS tags.*       ON TABLE memory TYPE string;
    DEFINE FIELD IF NOT EXISTS access_count ON TABLE memory TYPE int DEFAULT 0;
    DEFINE FIELD IF NOT EXISTS event_at     ON TABLE memory TYPE datetime;
    DEFINE FIELD IF NOT EXISTS created_at   ON TABLE memory TYPE datetime;
    DEFINE FIELD IF NOT EXISTS updated_at   ON TABLE memory TYPE datetime;
    DEFINE FIELD IF NOT EXISTS expires_at   ON TABLE memory TYPE option<datetime>;
    DEFINE FIELD IF NOT EXISTS domain       ON TABLE memory TYPE array;
    DEFINE FIELD IF NOT EXISTS domain.*     ON TABLE memory TYPE string;
    DEFINE FIELD IF NOT EXISTS visibility   ON TABLE memory TYPE string DEFAULT 'standard';
    DEFINE FIELD IF NOT EXISTS source_agent ON TABLE memory TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS session_id   ON TABLE memory TYPE option<string>;

    DEFINE INDEX IF NOT EXISTS idx_memory_type       ON TABLE memory FIELDS memory_type;
    DEFINE INDEX IF NOT EXISTS idx_memory_status     ON TABLE memory FIELDS status;
    DEFINE INDEX IF NOT EXISTS idx_memory_project    ON TABLE memory FIELDS project;
    DEFINE INDEX IF NOT EXISTS idx_memory_event      ON TABLE memory FIELDS event_at;
    DEFINE INDEX IF NOT EXISTS idx_memory_importance ON TABLE memory FIELDS importance;
    DEFINE INDEX IF NOT EXISTS idx_memory_domain     ON TABLE memory FIELDS domain;

    DEFINE ANALYZER IF NOT EXISTS memory_analyzer TOKENIZERS blank, class FILTERS lowercase, snowball(english);
    DEFINE INDEX IF NOT EXISTS idx_memory_summary_ft ON TABLE memory
        FIELDS summary FULLTEXT ANALYZER memory_analyzer BM25;
    DEFINE INDEX IF NOT EXISTS idx_memory_content_ft ON TABLE memory
        FIELDS content FULLTEXT ANALYZER memory_analyzer BM25;

    DEFINE TABLE IF NOT EXISTS entity SCHEMAFULL;
    DEFINE FIELD IF NOT EXISTS name       ON TABLE entity TYPE string;
    DEFINE FIELD IF NOT EXISTS kind       ON TABLE entity TYPE string;
    DEFINE FIELD IF NOT EXISTS roles      ON TABLE entity TYPE array;
    DEFINE FIELD IF NOT EXISTS roles.*    ON TABLE entity TYPE string;
    DEFINE FIELD IF NOT EXISTS attrs      ON TABLE entity TYPE object FLEXIBLE DEFAULT {};
    DEFINE FIELD IF NOT EXISTS context    ON TABLE entity TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS created_at ON TABLE entity TYPE datetime;
    DEFINE FIELD IF NOT EXISTS updated_at ON TABLE entity TYPE datetime;
    DEFINE INDEX IF NOT EXISTS idx_entity_name ON TABLE entity FIELDS name UNIQUE;
    DEFINE INDEX IF NOT EXISTS idx_entity_kind ON TABLE entity FIELDS kind;

    DEFINE TABLE IF NOT EXISTS person SCHEMAFULL;
    DEFINE FIELD IF NOT EXISTS name    ON TABLE person TYPE string;
    DEFINE FIELD IF NOT EXISTS role    ON TABLE person TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS context ON TABLE person TYPE option<string>;
    DEFINE INDEX IF NOT EXISTS idx_person_name ON TABLE person FIELDS name UNIQUE;

    DEFINE TABLE IF NOT EXISTS involves TYPE RELATION IN memory OUT entity SCHEMAFULL;
    DEFINE TABLE IF NOT EXISTS assigned TYPE RELATION IN memory OUT entity SCHEMAFULL;
    DEFINE TABLE IF NOT EXISTS belongs  TYPE RELATION IN memory OUT memory SCHEMAFULL;
    DEFINE TABLE IF NOT EXISTS updates  TYPE RELATION IN memory OUT memory SCHEMAFULL;
    DEFINE TABLE IF NOT EXISTS extends  TYPE RELATION IN memory OUT memory SCHEMAFULL;
    DEFINE TABLE IF NOT EXISTS derives  TYPE RELATION IN memory OUT memory SCHEMAFULL;
    DEFINE TABLE IF NOT EXISTS relates  TYPE RELATION IN memory OUT memory SCHEMAFULL;
"#;

// Unique indexes on edge tables prevent duplicate RELATE edges.
// RELATE is idempotent after this: a second RELATE on the same pair silently
// produces no new edge (checked in application code before calling RELATE).
const MIGRATION_002: &str = r#"
    DEFINE INDEX IF NOT EXISTS idx_involves_uniq ON TABLE involves FIELDS in, out UNIQUE;
    DEFINE INDEX IF NOT EXISTS idx_assigned_uniq ON TABLE assigned FIELDS in, out UNIQUE;
    DEFINE INDEX IF NOT EXISTS idx_updates_uniq  ON TABLE updates  FIELDS in, out UNIQUE;
    DEFINE INDEX IF NOT EXISTS idx_extends_uniq  ON TABLE extends  FIELDS in, out UNIQUE;
    DEFINE INDEX IF NOT EXISTS idx_derives_uniq  ON TABLE derives  FIELDS in, out UNIQUE;
    DEFINE INDEX IF NOT EXISTS idx_relates_uniq  ON TABLE relates  FIELDS in, out UNIQUE;
"#;

// Vector embedding field for semantic (cosine) similarity search.
// Default model: google/embeddinggemma-300m (768 native dims, MRL-truncated
// to 256 dims for 3x storage savings with minimal quality loss).
// Note: MTREE ANN index is not yet supported in SurrealKV embedded engine.
// Brute-force cosine scan via vector::similarity::cosine() is sufficient for
// a personal knowledge base with <100k records.
const MIGRATION_003: &str = r#"
    DEFINE FIELD IF NOT EXISTS vector ON TABLE memory TYPE option<array<float>>;
"#;

// SM-2 spaced repetition fields for exponential forgetting curve.
// - ease_factor: multiplier that grows on successful recall (default 2.5)
// - review_interval: days until next expected recall (default 1)
// - next_review_at: absolute time after which the memory is considered "overdue"
//   for access — pruning decays importance of overdue, unaccessed memories.
const MIGRATION_004: &str = r#"
    DEFINE FIELD IF NOT EXISTS ease_factor     ON TABLE memory TYPE option<float>;
    DEFINE FIELD IF NOT EXISTS review_interval ON TABLE memory TYPE option<int>;
    DEFINE FIELD IF NOT EXISTS next_review_at  ON TABLE memory TYPE option<datetime>;
"#;

// ── Public API ───────────────────────────────────────────────────────────────

/// Apply all pending migrations in version order.
///
/// Safe to call on every startup — already-applied migrations are skipped.
#[instrument(skip(db))]
pub async fn run(db: &SurrealClient) -> Result<()> {
    ensure_migration_table(db).await?;

    let current = current_version(db).await?;
    let pending: Vec<&Migration> = MIGRATIONS
        .iter()
        .filter(|m| m.version > current)
        .collect();

    if pending.is_empty() {
        info!(current_version = current, "schema up to date");
        return Ok(());
    }

    for migration in pending {
        info!(
            version = migration.version,
            description = migration.description,
            "applying migration"
        );

        db.query(migration.up)
            .await
            .map_err(|e| KbError::db(format!("migration {}: SQL error", migration.version), e))?;

        record_applied(db, migration.version, migration.description).await?;

        info!(version = migration.version, "migration applied");
    }

    Ok(())
}

/// Latest migration version defined in this binary.
pub fn latest_version() -> u32 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

// ── Internals ────────────────────────────────────────────────────────────────

async fn ensure_migration_table(db: &SurrealClient) -> Result<()> {
    db.query(
        "DEFINE TABLE IF NOT EXISTS _migration SCHEMAFULL;
         DEFINE FIELD IF NOT EXISTS version     ON TABLE _migration TYPE int;
         DEFINE FIELD IF NOT EXISTS description ON TABLE _migration TYPE string;
         DEFINE FIELD IF NOT EXISTS applied_at  ON TABLE _migration TYPE datetime;
         DEFINE INDEX IF NOT EXISTS idx_migration_version ON TABLE _migration FIELDS version UNIQUE;",
    )
    .await
    .map_err(|e| KbError::db("creating _migration table", e))?;
    Ok(())
}

async fn current_version(db: &SurrealClient) -> Result<u32> {
    let mut result = db
        .query("SELECT version FROM _migration ORDER BY version DESC LIMIT 1")
        .await
        .map_err(|e| KbError::db("querying migration version", e))?;

    let row: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing migration version", e))?;

    Ok(row
        .and_then(|v| v.get("version").and_then(|n| n.as_u64()))
        .unwrap_or(0) as u32)
}

async fn record_applied(db: &SurrealClient, version: u32, description: &str) -> Result<()> {
    let now = Utc::now();
    db.query(
        "CREATE _migration SET version = $version, description = $description, applied_at = $now",
    )
    .bind(("version", version as i64))
    .bind(("description", description.to_string()))
    .bind(("now", now))
    .await
    .map_err(|e| KbError::db(format!("recording migration {version}"), e))?;
    Ok(())
}
