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
    Migration {
        version: 5,
        description: "case-insensitive FTS index on entity.name for O(1) entity name matching",
        up: MIGRATION_005,
    },
    Migration {
        version: 6,
        description: "agent table for first-class authorship; author field + index on memory",
        up: MIGRATION_006,
    },
    Migration {
        version: 7,
        description: "bitemporal fields: valid_until, invalidated_at, invalidation_reason (ADR-011 Stage A)",
        up: MIGRATION_007,
    },
    Migration {
        version: 8,
        description: "retracts edge table for kb_retract audit trail (ADR-011 Stage D)",
        up: MIGRATION_008,
    },
    Migration {
        version: 9,
        description: "remove erroneous id-shadow on agent; backfill bitemporal fields on legacy archived rows",
        up: MIGRATION_009,
    },
    Migration {
        version: 10,
        description: "is_static flag for fast profile-style retrieval (ADR-013 supermemory parity)",
        up: MIGRATION_010,
    },
    Migration {
        version: 11,
        description: "composite index on memory.(invalidated_at, valid_until) for BITEMPORAL_ACTIVE predicate",
        up: MIGRATION_011,
    },
    Migration {
        version: 12,
        description: "typed_relation edge table for LLM-extracted typed relations between memories",
        up: MIGRATION_012,
    },
    Migration {
        version: 13,
        description: "entity soft-delete fields: archived_at, archive_reason, and index",
        up: MIGRATION_013,
    },
    Migration {
        version: 14,
        description: "anchor_entities field + BM25 fulltext index for write-time anchor extraction",
        up: MIGRATION_014,
    },
    Migration {
        version: 15,
        description: "_kb_metadata table for embedder fingerprint (provider/model/dim) — startup gate against silent embedding-space mismatch",
        up: MIGRATION_015,
    },
    Migration {
        version: 16,
        description: "channel + modality fields (ADR-019): orthogonal split of Source enum; old source column retained for read-compat one cycle",
        up: MIGRATION_016,
    },
    Migration {
        version: 17,
        description: "memory_analyzer_v2: add ascii filter for accent normalization; rebind summary/content/anchor FT indexes",
        up: MIGRATION_017,
    },
    Migration {
        version: 18,
        description: "consolidation_attempt table: idempotent ledger for auto-consolidation cluster tracking with cooldown support",
        up: MIGRATION_018,
    },
    Migration {
        version: 19,
        description: "extraction_status field: track post-create LLM sidecar extraction state per memory for observability and retry",
        up: MIGRATION_019,
    },
    Migration {
        version: 20,
        description: "unique composite index on typed_relation (in, out, kind) — DB-level TOCTOU guard against duplicate extraction edges",
        up: MIGRATION_020,
    },
    Migration {
        version: 21,
        description: "entity_alias relation table: per-alias bitemporal bookkeeping + DB-level UNIQUE + BM25 FTS for nickname/alias resolution (ADR-030)",
        up: MIGRATION_021,
    },
    Migration {
        version: 22,
        description: "feedback loop (B3): recall_event + feedback_event append-only logs, memory.salience prior",
        up: MIGRATION_022,
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

// Case-insensitive FTS index on entity.name to replace the full-table CONTAINS
// scan in match_known_entities. The class tokenizer splits on Unicode class
// boundaries (letter/digit runs), and lowercase normalises before indexing, so
// `name @@ "Alice"` matches "Alice Johnson" but not "Alexander".
// Migration 001 already defines a UNIQUE B-tree index `idx_entity_name`; this
// adds a parallel FTS index for the @@ operator — both can coexist.
const MIGRATION_005: &str = r#"
    DEFINE ANALYZER IF NOT EXISTS entity_name_analyzer
        TOKENIZERS class
        FILTERS lowercase;
    DEFINE INDEX IF NOT EXISTS idx_entity_name_ft ON TABLE entity
        FIELDS name
        FULLTEXT ANALYZER entity_name_analyzer BM25;
"#;

// Agent table for first-class authorship (ADR-010).
// `author` on memory is a nullable FK to agent — cheap indexed field predicate,
// not a graph traversal. NULL means user/me; Some(id) means agent-authored.
const MIGRATION_006: &str = r#"
    DEFINE TABLE IF NOT EXISTS agent SCHEMAFULL;

    DEFINE FIELD IF NOT EXISTS name         ON agent TYPE string;
    DEFINE FIELD IF NOT EXISTS kind         ON agent TYPE string;
    DEFINE FIELD IF NOT EXISTS model        ON agent TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS version      ON agent TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS parent_agent ON agent TYPE option<record<agent>>;
    DEFINE FIELD IF NOT EXISTS project      ON agent TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS config       ON agent TYPE option<object>;
    DEFINE FIELD IF NOT EXISTS status       ON agent TYPE string DEFAULT 'active';
    DEFINE FIELD IF NOT EXISTS spawned_at   ON agent TYPE datetime DEFAULT time::now();
    DEFINE FIELD IF NOT EXISTS retired_at   ON agent TYPE option<datetime>;
    DEFINE FIELD IF NOT EXISTS last_active  ON agent TYPE datetime DEFAULT time::now();

    DEFINE INDEX IF NOT EXISTS idx_agent_name    ON agent FIELDS name UNIQUE;
    DEFINE INDEX IF NOT EXISTS idx_agent_status  ON agent FIELDS status;
    DEFINE INDEX IF NOT EXISTS idx_agent_project ON agent FIELDS project;

    DEFINE FIELD IF NOT EXISTS author ON memory TYPE option<record<agent>>;
    DEFINE INDEX IF NOT EXISTS idx_memory_author ON memory FIELDS author;
"#;

// Bitemporal tracking fields for Phase 1 (ADR-011).
// valid_until: when a fact became false (NULL = still true, t_invalid analog)
// invalidated_at: when we stopped believing it (NULL = still believed, t_expired analog)
// invalidation_reason: why it was invalidated — stored as snake_case string, typed as InvalidationReason in Rust
const MIGRATION_007: &str = r#"
    DEFINE FIELD IF NOT EXISTS valid_until         ON memory TYPE option<datetime>;
    DEFINE FIELD IF NOT EXISTS invalidated_at      ON memory TYPE option<datetime>;
    DEFINE FIELD IF NOT EXISTS invalidation_reason ON memory TYPE option<string>;

    DEFINE INDEX IF NOT EXISTS idx_memory_valid_until    ON memory FIELDS valid_until;
    DEFINE INDEX IF NOT EXISTS idx_memory_invalidated_at ON memory FIELDS invalidated_at;
"#;

// `retracts` edge: correction -> retracts -> retracted (audit trail for kb_retract).
// Follows the same DEFINE TABLE TYPE RELATION + unique index pattern as all other
// edge tables (derives, updates, etc.) established in migrations 001 and 002.
const MIGRATION_008: &str = r#"
    DEFINE TABLE IF NOT EXISTS retracts TYPE RELATION IN memory OUT memory SCHEMAFULL;
    DEFINE INDEX IF NOT EXISTS idx_retracts_uniq ON TABLE retracts FIELDS in, out UNIQUE;
"#;

// Two corrections in one migration:
// 1. MIGRATION_006 erroneously declared `DEFINE FIELD id ON agent TYPE string` —
//    `id` is SurrealDB's implicit RecordId field and must not be redeclared.
//    Re-typing it as `string` clashes with the engine's RecordId storage.
//    REMOVE FIELD restores the implicit definition.
// 2. ADR-011 invariant — `status='archived'` iff `invalidated_at IS NOT NONE`.
//    Rows archived by code paths predating MIGRATION_007 (or by the now-fixed
//    `agent::retire(purge_memories=true)` path) have `status='archived'` but
//    `invalidated_at=NONE`, which means BITEMPORAL_ACTIVE reads still return
//    them as live. Backfill from `updated_at` with reason='forgotten'.
const MIGRATION_009: &str = r#"
    REMOVE FIELD IF EXISTS id ON agent;

    UPDATE memory SET
        invalidated_at      = updated_at,
        invalidation_reason = 'forgotten'
    WHERE status = 'archived' AND invalidated_at IS NONE;
"#;

// `is_static` boolean: true for permanent facts that don't decay (value, life_goal,
// fact, preference). Backfill at migration time from existing subtypes so legacy
// rows behave correctly with `kb_profile`. Indexed for O(1) profile filtering.
//
// Auto-set in `tools/remember.rs` based on subtype unless caller overrides.
// Static memories skip TTL, are excluded from prune decay, and are surfaced
// first in `kb_profile` regardless of recency.
const MIGRATION_010: &str = r#"
    DEFINE FIELD IF NOT EXISTS is_static ON TABLE memory TYPE bool DEFAULT false;
    DEFINE INDEX IF NOT EXISTS idx_memory_is_static ON TABLE memory FIELDS is_static;

    UPDATE memory SET is_static = true
    WHERE subtype IN ['value', 'life_goal', 'preference']
       OR (subtype = 'fact' AND importance >= 4);
"#;

// Composite index covering both columns of the BITEMPORAL_ACTIVE predicate:
//   invalidated_at IS NONE AND (valid_until IS NONE OR valid_until > time::now())
// A single composite index lets SurrealDB satisfy the leading equality term
// (invalidated_at = NONE) and the range scan on valid_until in one pass.
const MIGRATION_011: &str = r#"
    DEFINE INDEX IF NOT EXISTS memory_bitemporal_idx ON TABLE memory COLUMNS invalidated_at, valid_until;
"#;

// LLM-extracted typed relations between memories.
// Separate from the untyped `relates` edge (auto-linked by cosine similarity)
// because typed_relation carries confidence, provenance, and a controlled vocabulary
// of causal/temporal/semantic relation kinds — richer signal for spreading activation.
const MIGRATION_012: &str = r#"
    DEFINE TABLE IF NOT EXISTS typed_relation TYPE RELATION IN memory OUT memory SCHEMAFULL;

    DEFINE FIELD IF NOT EXISTS in               ON TABLE typed_relation TYPE record<memory>;
    DEFINE FIELD IF NOT EXISTS out              ON TABLE typed_relation TYPE record<memory>;
    DEFINE FIELD IF NOT EXISTS kind             ON TABLE typed_relation TYPE string
        ASSERT $value INSIDE ['causes', 'enables', 'blocks', 'follows', 'precedes', 'similar_to', 'contradicts', 'mentions', 'depends_on', 'part_of'];
    DEFINE FIELD IF NOT EXISTS confidence       ON TABLE typed_relation TYPE float
        ASSERT $value >= 0.0 AND $value <= 1.0;
    DEFINE FIELD IF NOT EXISTS extracted_at     ON TABLE typed_relation TYPE datetime DEFAULT time::now();
    DEFINE FIELD IF NOT EXISTS extractor_version ON TABLE typed_relation TYPE string;
    DEFINE FIELD IF NOT EXISTS source_memory_id ON TABLE typed_relation TYPE record<memory>;
    DEFINE FIELD IF NOT EXISTS retracted_at     ON TABLE typed_relation TYPE option<datetime>;
    DEFINE FIELD IF NOT EXISTS retraction_reason ON TABLE typed_relation TYPE option<string>;

    DEFINE INDEX IF NOT EXISTS typed_relation_kind_idx   ON TABLE typed_relation COLUMNS kind;
    DEFINE INDEX IF NOT EXISTS typed_relation_in_idx     ON TABLE typed_relation COLUMNS in;
    DEFINE INDEX IF NOT EXISTS typed_relation_out_idx    ON TABLE typed_relation COLUMNS out;
    DEFINE INDEX IF NOT EXISTS typed_relation_source_idx ON TABLE typed_relation COLUMNS source_memory_id;
"#;

// Entity soft-delete tombstone fields.
// Archived entities are hidden from all normal read paths via `archived_at IS NONE`.
// Never hard-delete — tombstone stays in DB forever.
const MIGRATION_013: &str = r#"
    DEFINE FIELD IF NOT EXISTS archived_at    ON TABLE entity TYPE option<datetime>;
    DEFINE FIELD IF NOT EXISTS archive_reason ON TABLE entity TYPE option<string>;
    DEFINE INDEX IF NOT EXISTS entity_archived_idx ON TABLE entity COLUMNS archived_at;
"#;

// Write-time anchor entities: top-3 proper nouns / project names / technology names
// extracted from the memory summary by the LLM sidecar. Stored as array<string>
// and indexed with BM25 so recall can match on anchors without a runtime LLM call.
// Populated asynchronously by `llm::extractor::extract_anchors_and_persist`;
// NULL until the sidecar has run for that memory.
const MIGRATION_014: &str = r#"
    DEFINE FIELD IF NOT EXISTS anchor_entities ON TABLE memory TYPE option<array<string>>;
    DEFINE INDEX IF NOT EXISTS idx_memory_anchor_ft ON TABLE memory FIELDS anchor_entities FULLTEXT ANALYZER memory_analyzer BM25;
"#;

const MIGRATION_015: &str = r#"
    DEFINE TABLE IF NOT EXISTS _kb_metadata SCHEMAFULL;
    DEFINE FIELD IF NOT EXISTS key        ON TABLE _kb_metadata TYPE string;
    DEFINE FIELD IF NOT EXISTS value      ON TABLE _kb_metadata TYPE string;
    DEFINE FIELD IF NOT EXISTS updated_at ON TABLE _kb_metadata TYPE datetime;
    DEFINE INDEX IF NOT EXISTS idx_kb_metadata_key ON TABLE _kb_metadata FIELDS key UNIQUE;
"#;

// ADR-019: orthogonal Channel + Modality fields replacing the monolithic Source enum.
// `source` is relaxed to option<string> here so new writes that omit it (the Rust
// code no longer provides it) satisfy the SCHEMAFULL schema. The column is retained
// for one release cycle for read-compat; it will be removed in the next cycle.
//
// Backfill mapping (conservative — unknown source values are left as NONE):
//   manual  → channel=cli,     modality=text
//   meeting → channel=NONE,    modality=transcript  (channel unknown)
//   slack   → channel=slack,   modality=text
//   discord → channel=discord, modality=text
//   code    → channel=cli,     modality=code
//   chat    → channel=api,     modality=text
const MIGRATION_016: &str = r#"
    DEFINE FIELD IF NOT EXISTS channel  ON TABLE memory TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS modality ON TABLE memory TYPE option<string>;
    DEFINE FIELD OVERWRITE source ON TABLE memory TYPE option<string>;

    UPDATE memory SET channel = 'cli',     modality = 'text'       WHERE source = 'manual'  AND channel IS NONE;
    UPDATE memory SET                      modality = 'transcript'  WHERE source = 'meeting' AND modality IS NONE;
    UPDATE memory SET channel = 'slack',   modality = 'text'       WHERE source = 'slack'   AND channel IS NONE;
    UPDATE memory SET channel = 'discord', modality = 'text'       WHERE source = 'discord' AND channel IS NONE;
    UPDATE memory SET channel = 'cli',     modality = 'code'       WHERE source = 'code'    AND channel IS NONE;
    UPDATE memory SET channel = 'api',     modality = 'text'       WHERE source = 'chat'    AND channel IS NONE;
"#;

// Improved BM25 analyzer with ASCII normalization.
//
// memory_analyzer (v1, migration 001) used: blank, class tokenizers + lowercase, snowball(english).
// This migration adds `ascii` as the first filter so accented chars (é → e, ü → u) are
// normalized before lowercasing and stemming, improving cross-language recall for names
// that appear with and without diacritics.
//
// SurrealDB v3 provides a `mapper(path)` filter that can act as a stopword map, but it
// requires an external file at a known filesystem path — unsuitable for an embedded server
// with no guaranteed working directory. Application-layer stopword pruning is deferred to a
// future ADR if BM25 noise from generic terms ("the", "is", "memory", "note") is measured
// as a significant quality regression.
//
// REDEFINE (not IF NOT EXISTS) is intentional — we are re-binding existing indexes to the
// new analyzer. The old `memory_analyzer` is left in place so entity_name_analyzer (which
// correctly never used it) and any other references remain valid. The three FT indexes on
// memory.summary, memory.content, and memory.anchor_entities are the only ones that
// reference memory_analyzer and are safe to redefine here.
const MIGRATION_017: &str = r#"
    DEFINE ANALYZER IF NOT EXISTS memory_analyzer_v2
        TOKENIZERS blank, class
        FILTERS ascii, lowercase, snowball(english);

    DEFINE INDEX OVERWRITE idx_memory_summary_ft ON TABLE memory
        FIELDS summary FULLTEXT ANALYZER memory_analyzer_v2 BM25;
    DEFINE INDEX OVERWRITE idx_memory_content_ft ON TABLE memory
        FIELDS content FULLTEXT ANALYZER memory_analyzer_v2 BM25;
    DEFINE INDEX OVERWRITE idx_memory_anchor_ft ON TABLE memory
        FIELDS anchor_entities FULLTEXT ANALYZER memory_analyzer_v2 BM25;
"#;

// Consolidation attempt ledger for the auto-consolidation loop.
// cluster_hash is the stable dedup key (SHA-256 of sorted source IDs joined by \n).
// skip_until lets the loop skip clusters that were recently evaluated without re-running
// the LLM sidecar. UPSERT-on-conflict pattern increments attempt_count each cycle.
const MIGRATION_018: &str = r#"
    DEFINE TABLE IF NOT EXISTS consolidation_attempt SCHEMAFULL;
    DEFINE FIELD IF NOT EXISTS cluster_hash  ON consolidation_attempt TYPE string;
    DEFINE FIELD IF NOT EXISTS source_ids    ON consolidation_attempt TYPE array<record<memory>>;
    DEFINE FIELD IF NOT EXISTS attempted_at  ON consolidation_attempt TYPE datetime;
    DEFINE FIELD IF NOT EXISTS last_outcome  ON consolidation_attempt TYPE string;
    DEFINE FIELD IF NOT EXISTS last_reason   ON consolidation_attempt TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS merged_into   ON consolidation_attempt TYPE option<record<memory>>;
    DEFINE FIELD IF NOT EXISTS skip_until    ON consolidation_attempt TYPE datetime;
    DEFINE FIELD IF NOT EXISTS attempt_count ON consolidation_attempt TYPE int DEFAULT 0;
    DEFINE INDEX IF NOT EXISTS idx_consolidation_cluster_hash ON consolidation_attempt FIELDS cluster_hash UNIQUE;
    DEFINE INDEX IF NOT EXISTS idx_consolidation_skip_until   ON consolidation_attempt FIELDS skip_until;
"#;

// extraction_status field: tracks whether the post-create LLM sidecar pipeline
// (anchor entities + typed relations) has been attempted and whether it succeeded.
// Possible values mirror the ExtractionStatus enum: pending, complete, anchor_failed,
// relation_failed, both_failed, sidecar_unavailable, skipped.
//
// Existing rows get 'pending' as a neutral backfill — they predate this field and may
// already have anchors set; the --retry-failed-extraction flag uses `is_retryable()`
// which excludes 'pending' (and 'complete', 'skipped') from retry.
//
// The index enables O(1) retry-candidate queries without a full table scan.
const MIGRATION_019: &str = r#"
    DEFINE FIELD IF NOT EXISTS extraction_status ON TABLE memory TYPE option<string>;
    DEFINE INDEX IF NOT EXISTS idx_memory_extraction_status ON TABLE memory FIELDS extraction_status;

    UPDATE memory SET extraction_status = 'pending'
    WHERE extraction_status IS NONE AND status = 'active';
"#;

// Unique composite index on typed_relation prevents TOCTOU duplicate extraction
// edges even under concurrent post-create pipelines. The application-layer
// pre-check in typed_relation::upsert stays as a fast-path skip; this becomes
// the correctness guarantee.
const MIGRATION_020: &str = r#"
    DEFINE INDEX IF NOT EXISTS idx_typed_relation_uniq
        ON TABLE typed_relation
        COLUMNS in, out, kind
        UNIQUE;
"#;

// entity_alias: regular SCHEMAFULL table (not a RELATION) with an entity_id field.
// RELATE-based approach was rejected because SurrealDB v3 treats `none` as a reserved
// keyword (null literal), making `RELATE entity -> entity_alias -> none` a parse error.
// A plain table with a record-link field is equivalent for our access patterns — we never
// traverse the graph direction from entity_alias, we always SELECT by entity_id or value.
const MIGRATION_021: &str = r#"
    DEFINE TABLE IF NOT EXISTS entity_alias SCHEMAFULL;
    DEFINE FIELD IF NOT EXISTS entity_id   ON entity_alias TYPE record<entity>;
    DEFINE FIELD IF NOT EXISTS `value`     ON entity_alias TYPE string ASSERT string::len($value) >= 3;
    DEFINE FIELD IF NOT EXISTS valid_from  ON entity_alias TYPE datetime DEFAULT time::now();
    DEFINE FIELD IF NOT EXISTS valid_until ON entity_alias TYPE option<datetime>;
    DEFINE FIELD IF NOT EXISTS added_by    ON entity_alias TYPE option<record<agent>>;
    DEFINE FIELD IF NOT EXISTS archived_at ON entity_alias TYPE option<datetime>;
    DEFINE INDEX IF NOT EXISTS idx_alias_value_unique ON entity_alias FIELDS `value` UNIQUE;
    DEFINE INDEX IF NOT EXISTS idx_alias_entity_id ON entity_alias FIELDS entity_id;
    DEFINE ANALYZER IF NOT EXISTS alias_analyzer TOKENIZERS blank FILTERS lowercase;
    DEFINE INDEX IF NOT EXISTS idx_alias_value_ft ON entity_alias FIELDS `value` FULLTEXT ANALYZER alias_analyzer BM25;
"#;

// B3 feedback loop. Both event tables are append-only and carry ids only —
// never query or memory text (privacy). `salience` is a decayed running sum of
// feedback, used only as an optional ranking prior; it never gates or archives.
const MIGRATION_022: &str = r#"
    DEFINE TABLE IF NOT EXISTS recall_event SCHEMAFULL;
    DEFINE FIELD IF NOT EXISTS trace_id        ON recall_event TYPE string;
    DEFINE FIELD IF NOT EXISTS at              ON recall_event TYPE datetime DEFAULT time::now();
    DEFINE FIELD IF NOT EXISTS actor           ON recall_event TYPE option<record<agent>>;
    DEFINE FIELD IF NOT EXISTS memory_ids      ON recall_event TYPE array<string>;
    DEFINE FIELD IF NOT EXISTS tokens_returned ON recall_event TYPE int;
    DEFINE INDEX IF NOT EXISTS idx_recall_event_trace ON recall_event FIELDS trace_id UNIQUE;
    DEFINE INDEX IF NOT EXISTS idx_recall_event_at    ON recall_event FIELDS at;

    DEFINE TABLE IF NOT EXISTS feedback_event SCHEMAFULL;
    DEFINE FIELD IF NOT EXISTS trace_id  ON feedback_event TYPE option<string>;
    DEFINE FIELD IF NOT EXISTS memory_id ON feedback_event TYPE string;
    DEFINE FIELD IF NOT EXISTS signal    ON feedback_event TYPE string;
    DEFINE FIELD IF NOT EXISTS at        ON feedback_event TYPE datetime DEFAULT time::now();
    DEFINE FIELD IF NOT EXISTS actor     ON feedback_event TYPE option<record<agent>>;
    DEFINE INDEX IF NOT EXISTS idx_feedback_memory ON feedback_event FIELDS memory_id;

    DEFINE FIELD IF NOT EXISTS salience    ON memory TYPE float DEFAULT 0.0;
    DEFINE FIELD IF NOT EXISTS salience_at ON memory TYPE option<datetime>;
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
