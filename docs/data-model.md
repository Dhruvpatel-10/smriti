# Data Model

## Memory Record

Every memory has:
- **summary** — 10-500 chars. Injected on retrieval. REQUIRED.
- **content** — full content. Fetched only on `kb_drill()`. Optional.
- **source_ref** — path/ID of original raw file. Never injected.
- **memory_type** + **subtype** — from the 23-subtype taxonomy
- **source** — input channel: manual, meeting, slack, discord, code, chat
- **project** — primary project name (optional)
- **domain** — life areas: health, learning, philosophy, etc. (free-form, multi-valued)
- **people** — resolved to entity records via `involves` edges
- **importance** — 1-5, with per-subtype floors/ceilings
- **visibility** — standard, private, sensitive
- **status** — active, archived, superseded
- **event_at** — WHEN it happened (bi-temporal: distinct from created_at)
- **valid_until** — `Option<DateTime<Utc>>` — when the fact became false; NULL = still true (index: idx_memory_valid_until)
- **created_at** / **updated_at** — storage timestamps
- **expires_at** — TTL for working/* subtypes
- **invalidated_at** — `Option<DateTime<Utc>>` — when we stopped believing it (set by supersession, retraction, TTL, forget); NULL = still believed (index: idx_memory_invalidated_at)
- **invalidation_reason** — `Option<InvalidationReason>` — one of: `Superseded`, `Retracted`, `Expired`, `Forgotten`, `Consolidated`
- **access_count** — incremented on every recall hit
- **tags** — free-form categorization
- **source_agent** / **session_id** — provenance tracking
- **author** — `Option<AgentId>` — FK to `agent` table; `NULL` = user/me, `Some` = agent that wrote the memory; indexed as `idx_memory_author`
- **anchor_entities** — `Option<Vec<String>>` — up to 3 named entities (proper nouns, project names, technology names, event names) extracted from the summary at write time by the LLM sidecar. Stored raw (no lowercasing, no stemming) — the SurrealDB BM25 analyzer (lowercase + snowball) normalises at index time. NULL until the async extraction completes. Indexed via `idx_memory_anchor_ft` (BM25 fulltext); the BM25 query in `kb_recall` and `kb_recall_at` includes an `OR anchor_entities @@ $query` clause. Not surfaced in `MemorySummary` or MCP recall responses — internal index field only.

## Importance Floors/Ceilings

| Subtype | Floor | Ceiling | Rationale |
|---------|-------|---------|-----------|
| decision | 4 | 5 | Decisions are permanent |
| value | 5 | 5 | Core beliefs |
| life_goal | 5 | 5 | Life goals |
| action_item | 3 | 5 | Tasks must surface |
| goal | 4 | 5 | Goals matter |
| scratch | 1 | 2 | Ephemeral |
| daily_log | 1 | 2 | Context, not critical |
| (others) | 1 | 5 | No enforcement |

## Entity System

Flexible, schemaless entities:
```
entity {
  name: String,           // unique key
  kind: String,           // person, company, product, concept, project
  roles: Vec<String>,     // investor, advisor, customer, etc.
  attrs: Object,          // freeform key-value (includes last_contact for persons)
  context: Option<String>,// one-line summary
}
```

- `attrs.last_contact` auto-updated when entity is linked to a memory via `link_entity`
- Stale contact alerts fire when `last_contact` exceeds configured threshold
- Projects can be entities with lifecycle attrs (status, killed_reason, pivoted_to)

**`EntityFull`** (returned by `kb_entity_get`) and **`EntitySummary`** (returned by `kb_entity_find`) both carry `aliases: Vec<String>` — the active (non-archived) alias values for that entity.

## entity_alias Table (Migration 021, ADR-030)

A separate SCHEMAFULL table — NOT a RELATE edge — that maps alias strings to canonical entities. Chosen over an `aliases: array<string>` field on `entity` to enable DB-level per-element UNIQUE, per-element bitemporal timestamps, and clean per-element FTS scoring.

```surql
DEFINE TABLE entity_alias SCHEMAFULL;
DEFINE FIELD entity_id   ON entity_alias TYPE record<entity>;
DEFINE FIELD `value`     ON entity_alias TYPE string ASSERT string::len($value) >= 3;
DEFINE FIELD valid_from  ON entity_alias TYPE datetime DEFAULT time::now();
DEFINE FIELD valid_until ON entity_alias TYPE option<datetime>;
DEFINE FIELD added_by    ON entity_alias TYPE option<record<agent>>;
DEFINE FIELD archived_at ON entity_alias TYPE option<datetime>;
DEFINE INDEX idx_alias_value_unique ON entity_alias FIELDS `value` UNIQUE;
DEFINE INDEX idx_alias_entity_id    ON entity_alias FIELDS entity_id;
DEFINE ANALYZER alias_analyzer TOKENIZERS blank FILTERS lowercase;
DEFINE INDEX idx_alias_value_ft     ON entity_alias FIELDS `value` FULLTEXT ANALYZER alias_analyzer BM25;
```

Key constraints:
- `value` is a reserved keyword in SurrealDB v3 (single-field projection shorthand); must be backtick-escaped as `` `value` `` in all SurrealQL.
- `UNIQUE` on `idx_alias_value_unique` — alias namespace is global; the same alias string cannot point to two canonical entities.
- Minimum alias length: 3 characters (DB-level ASSERT).
- `blank` tokenizer — splits on whitespace only; does not break short names like "Bo" or "Al" into sub-tokens that generate false positives.
- All reads filter `archived_at IS NONE` (soft-delete invariant, same pattern as `memory` and `entity`).
- Bitemporal: `valid_from`/`valid_until` make alias membership a first-class temporal fact; `kb_recall_at` can reconstruct the alias set active at any `as_of` timestamp via `entity_alias::resolve_at`.

## Graph Edges

```
memory  ->involves->  entity       (mentioned, present at meeting)
memory  ->assigned->  entity       (action item assignee — auto-wired for action_item subtype)
memory  ->updates->   memory       (supersession — old marked as superseded)
memory  ->extends->   memory       (adds to without contradicting; chunk rows use OUTBOUND extends to parent: `RELATE chunk -> extends -> parent`)
memory  ->derives->   memory       (consolidation provenance)
memory  ->relates->   memory       (semantic similarity)
memory  ->retracts->  memory       (explicit correction — correction memory points to the retracted memory)
```

**`retracts` edge (Migration 008):** Created by `kb_archive(id, reason=retract, correction_id=X)`. The correction memory `X` is the source; the wrongly-believed memory is the target. No edge-level `created_at` — audit order is established via the correction memory's own `created_at`.

### Auto-Link Thresholds

| Condition | Threshold | Edge |
|-----------|-----------|------|
| Same project, cosine > 0.90 | 0.90 | `extends` |
| Same project/domain, cosine > 0.75 | 0.75 | `relates` |
| Cross-domain, cosine > 0.85 | 0.85 | `relates` |

- 8 similarity edges max per memory (involves edges uncapped)
- Entity name matching: whole-word boundary, min 4 chars

## Visibility

Enforced in ALL query paths:
- `standard` — normal retrieval, appears in markdown layer
- `private` — only on explicit domain/direct query, never cross-project
- `sensitive` — never in markdown, never logged, query-only with explicit ID/domain

## Decay & Pruning

**TTL expiry** — working/* subtypes have mandatory TTLs (scratch=48h, priority=7d, etc.)

**SM-2 decay** — memories past review date with no access lose importance. Excludes:
- Importance ≥ 5 (immune)
- Subtypes: decision, value, life_goal (immune regardless of importance)
- Working/* (TTL handles them)

**Domain-aware rates** — health/fitness at 0.5x, philosophy/personal at 0.25x

**Archive threshold** — importance falls to configured min → archived

**Supersession** — `updates_id` marks old memory as superseded (verified before executing)

Rule: **never hard-delete**. `BITEMPORAL_ACTIVE` predicate (`invalidated_at IS NONE AND (valid_until IS NONE OR valid_until > time::now())`) filters archived/superseded at all read sites. `WHERE status = 'active'` is retained as a derived index hint kept in sync with `invalidated_at`.

## Agent Table

Added in Migration 006 (ADR-010). Agents are first-class authors — peer table to `memory` and `entity`.

| Field | Type | Description |
|-------|------|-------------|
| `id` | `string` | Stable `AgentId` (newtype) |
| `name` | `string` | Unique human-readable handle (UNIQUE index) |
| `kind` | `string` | One of: `assistant`, `reviewer`, `summarizer`, `researcher`, `custom` |
| `model` | `option<string>` | LLM model identifier (e.g. `"gemini-2.0-flash"`) |
| `version` | `option<string>` | Caller-supplied version string |
| `parent_agent` | `option<record<agent>>` | FK to parent agent for version chains |
| `project` | `option<string>` | Scopes agent to a project; inherited by memories |
| `config` | `option<object>` | Freeform configuration blob |
| `status` | `string` | `active` or `archived`; DEFAULT `'active'` |
| `spawned_at` | `datetime` | Creation timestamp; DEFAULT `time::now()` |
| `retired_at` | `option<datetime>` | Set on soft-retirement; NULL while active |
| `last_active` | `datetime` | Touched on every `kb_remember` with this agent; DEFAULT `time::now()` |

**Indexes:** `idx_agent_name` (UNIQUE), `idx_agent_status`, `idx_agent_project`

**Resurrection:** re-registering an archived agent name updates the row in-place (status cleared, mutable fields refreshed). Old memories retain their `author` FK and will reflect the resurrected agent's new `kind`/`model` when drilled.

**Relationship to memory:** `memory.author` is a FK (`option<record<agent>>`). `NULL` means the memory was written by the human user directly. Dedup is scoped per-author — cross-author near-duplicates (cosine 0.82–0.92) receive a `relates` edge instead of rejection.

## Single-User Invariant & Future Migration

The current schema preserves the single-user invariant: there is no `owner_user_id` column and no ACL machinery. When multi-user support is needed, the upgrade path is a single column addition (`owner_user_id: UserId`) to `agent`, `memory`, and `entity`, with every query scoped by owner — no schema upheaval required (per ADR-010).

## Bi-Temporal Timestamps (Graphiti mapping)

| Graphiti | smriti | Meaning |
|---|---|---|
| t_valid   | `event_at`         | When the fact became true |
| t_invalid | `valid_until`      | When the fact became false; NULL = still true |
| t_created | `created_at`       | When stored in DB |
| t_expired | `invalidated_at`   | When we stopped believing it |

- Temporal queries (`kb_recall_at`) use all four fields — see architecture.md for the 4-clause predicate.
- `invalidation_reason` values: `Superseded` (updates_id), `Retracted` (kb_archive reason=retract), `Expired` (TTL), `Forgotten` (kb_archive reason=forget), `Consolidated` (kb_consolidate / auto-consolidation).

## typed_relation Edge Table (Migration 012)

LLM-extracted causal/semantic/temporal relations between memories. Distinct from the untyped `relates` edge (auto-linked by cosine threshold) because it carries:

- **kind** — controlled vocabulary: `causes`, `enables`, `blocks`, `follows`, `precedes`, `similar_to`, `contradicts`, `mentions`, `depends_on`, `part_of`
- **confidence** — float 0.0–1.0 from the extracting model
- **extractor_version** — model/version that produced the edge (provenance)
- **source_memory_id** — the memory whose content was analysed to derive the edge
- **extracted_at** — timestamp of extraction; DEFAULT `time::now()`
- **retracted_at** / **retraction_reason** — soft-invalidation without touching the source memories

Indexed on `kind`, `in`, `out`, `source_memory_id` for spreading-activation traversal.
Why separate from `relates`: `relates` is created automatically at write time from embedding cosine similarity; `typed_relation` is written by an LLM extraction pass and must carry confidence + provenance to be trustworthy enough for reasoning. Mixing them would dilute both signals.

## consolidation_attempt Table (Migration 018)

Ledger of auto-consolidation attempts produced by the periodic loop (ADR-026). One row
per unique candidate cluster, identified by a stable `cluster_hash`. Prevents redundant
sidecar calls via a `skip_until` cooldown fence.

| Field | Type | Description |
|-------|------|-------------|
| `cluster_hash` | `string` | SHA-256 of sorted source memory IDs; UNIQUE index |
| `source_ids` | `array<record<memory>>` | Source memory record links evaluated in this attempt |
| `attempted_at` | `datetime` | Timestamp of last attempt |
| `last_outcome` | `string` | `Merged` \| `DeferredLowConfidence` \| `DeferredOversized` \| `DeferredSensitive` \| `SidecarError` \| `MergeFailed` |
| `last_reason` | `option<string>` | LLM-supplied reason string, or error description |
| `merged_into` | `option<record<memory>>` | FK to the resulting consolidated memory; set on `Merged` outcome only |
| `skip_until` | `datetime` | Cooldown fence; loop skips this cluster until `skip_until > time::now()` |
| `attempt_count` | `int` | Cumulative number of attempts on this cluster hash |

**Indexes:** `idx_consolidation_attempt_hash` (UNIQUE on `cluster_hash`), `idx_consolidation_attempt_skip_until`.

All queries against this table use parameterized bindings. `fetch_source_metadata` applies
`BITEMPORAL_ACTIVE` predicate before sending summaries to the sidecar (defense in depth
against merging already-archived sources).
