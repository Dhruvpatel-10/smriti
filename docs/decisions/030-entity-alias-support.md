# ADR-030: Entity Alias Support

**Date:** 2026-05-02
**Status:** Proposed
**Extends:** ADR-014 (anchor extraction), ADR-022 (adaptive reranker policy), ADR-023 (per-edge spreading activation), ADR-029 (structural hardening hub cap)

## Context

Entities currently have a single `name: String` UNIQUE field. Three independent retrieval
paths break when a person or entity is known under multiple forms:

1. **`kb_recall person="Bob"`** — person-filter does an exact lowercase match against
   `entity.name`. A search for "Bob" misses the canonical "Robert Smith" record entirely.
2. **`kb_entity_find`** — substring `CONTAINS` on `name`. Misses any token absent from
   the canonical string; searching "Bob" against "Robert Smith" returns nothing.
3. **Auto-link (`auto_link.rs:106`)** — FTS `@@` token match on `name`. The comment is
   explicit: *"Alex will not match Alexander."* Alias tokens are invisible to the matcher,
   so memories that reference a nickname never acquire the `involves` edge to the
   canonical entity. Those memories are therefore unreachable via entity-anchored recall
   and spreading activation.

Free-text recall (BM25 + vector) partially compensates when an alias appears verbatim in
a memory summary, but this provides no entity anchoring and no spreading activation through
the entity graph — the two mechanisms that propagate relevance across related memories.

SurrealDB v3 offers no built-in synonym filter in `DEFINE ANALYZER`; available filters are
`ascii`, `lowercase`, `uppercase`, `edgengram`, `ngram`, `snowball`, and `mapper`. The
`mapper(path)` filter supports a static dictionary but does not support multi-canonical
resolution, and its behavior on multi-token values is unconfirmed. Synonym resolution must
therefore be handled at the storage level rather than the index level.

## Decision

Add a separate `entity_alias` relation table. Each row is an edge from an `entity` to
`none` (a unary relation) carrying a single alias string with full bitemporal bookkeeping
and a DB-level UNIQUE constraint.

### Schema (Migration 021)

```surql
DEFINE TABLE entity_alias SCHEMAFULL;
DEFINE FIELD entity_id   ON entity_alias TYPE record<entity>;
DEFINE FIELD `value`     ON entity_alias TYPE string ASSERT string::len($value) >= 3;
DEFINE FIELD valid_from  ON entity_alias TYPE datetime DEFAULT time::now();
DEFINE FIELD valid_until ON entity_alias TYPE option<datetime>;
DEFINE FIELD added_by    ON entity_alias TYPE option<record<agent>>;
DEFINE FIELD archived_at ON entity_alias TYPE option<datetime>;
DEFINE INDEX idx_alias_value_unique ON entity_alias FIELDS `value` UNIQUE;
DEFINE INDEX idx_alias_entity_id ON entity_alias FIELDS entity_id;
DEFINE ANALYZER alias_analyzer TOKENIZERS blank FILTERS lowercase;
DEFINE INDEX idx_alias_value_ft ON entity_alias FIELDS `value` FULLTEXT ANALYZER alias_analyzer BM25;
```

**Implementation note:** the original design used `DEFINE TABLE entity_alias TYPE RELATION IN entity OUT none`.
This was rejected at implementation time because SurrealDB v3 treats `none` as a reserved keyword (null
literal), so `RELATE entity -> entity_alias -> none SET ...` is a parse error.  The `value` field name is
also reserved in SurrealDB v3 (single-field projection shorthand) and must be backtick-escaped in all
SurrealQL: `` `value` ``.  A plain SCHEMAFULL table with a `record<entity>` link field is equivalent
for our access patterns — we always SELECT by `entity_id` or `` `value` ``, never traverse the graph
direction from `entity_alias`.

A separate table is chosen over `aliases: array<string>` on the entity record for five
independent reasons:

- **DB-level UNIQUE.** Prevents TOCTOU races in upsert collision checks; an array field
  cannot carry a per-element unique constraint in SurrealDB.
- **Bitemporal.** `valid_from`/`valid_until` make aliases first-class bitemporal facts,
  correct for `kb_recall_at` point-in-time queries. Array elements have no individual
  timestamps.
- **Soft-delete invariant.** `archived_at` follows the same tombstone pattern used by
  memories and entities; no hard deletes.
- **Per-element FTS.** A `FULLTEXT` index on a `string` field is clean. BM25 scoring
  on an `array<string>` field aggregates across all elements, producing distorted scores
  for entities with many aliases.
- **Tokenizer choice.** `blank` tokenizer splits only on whitespace. `class`-based
  tokenizers break short names ("Bo", "Al") into sub-tokens that generate false positives
  across unrelated entities. The 3-character DB-level minimum (`ASSERT string::len($value) >= 3`)
  provides an additional false-positive guard.

### API changes

**`kb_entity_upsert`** gains an optional `aliases` parameter:

```
kb_entity_upsert(name, kind, roles?, attrs?, context?, aliases?: Vec<String>) -> EntityId
```

Server-side processing: lowercase each alias, deduplicate within the call, validate
`len >= 3`, check the unique index for collisions against other entities, write
`entity_alias` rows with `added_by = source_agent`.

**`EntityFull`** gains `aliases: Vec<String>` — active (non-archived) alias values,
returned by `kb_entity_get`.

### Resolution path changes

All four code paths that currently match only on `entity.name` are extended to OR-include
a join against `entity_alias`:

- `get_by_name` — exact match: `entity.name = $v OR entity_alias.value = $v`
- `kb_entity_find` — substring/FTS: also queries `idx_alias_value_ft`; results
  deduplicated to canonical entity before return.
- `link_entity` / `match_known_entities` (`auto_link.rs`) — FTS `@@` extended to also
  hit `idx_alias_value_ft`; the canonical entity ID is used for the `involves` edge.
- **Person filter** (`search.rs`) — `kb_recall person=` lookup extended to alias index;
  returns canonical entity ID for downstream spreading activation.

### Alias expansion in free-text recall

Before the BM25/vector stage, run an alias FTS lookup on the query string. Any matched
canonical entities are injected as entity-anchor candidates for the person filter and
spreading activation stages. This closes the gap where alias text appears in the query
but the caller has not explicitly passed `person=`.

### Disambiguation

When an alias value resolves to more than one canonical entity (possible only if the
UNIQUE constraint is relaxed in a future migration, or via a DB inconsistency), the server
WARN-logs the collision and returns all candidate entities in the tool response. There is
no silent recency-pick.

### Out of scope for v1

- Phonetic / fuzzy / Levenshtein matching ("Bob" ↔ "Robert" have edit distance 5;
  false-positive risk is high and the correct threshold is corpus-dependent).
- Alias support for the legacy `person` table — folded into the entity migration
  covered by a separate ADR.
- Multi-user (stark/leanscale) author-scoped entity resolution — the alias namespace
  is currently global; per-author scoping is deferred to the auth-model ADR.
- **`kb_recall_at` person filter via `resolve_at`**: `recall_at` does not yet accept a
  `person=` parameter, so `entity_alias::resolve_at` is not wired into the search path.
  The function exists and is tested directly.  When `kb_recall_at` gains `person=`,
  route through `resolve_at` instead of `resolve` to get the alias set active at `as_of`.

## Consequences

### Positive

- All three broken retrieval paths are fixed: exact person-filter, substring entity-find,
  and auto-link FTS all resolve nicknames to canonical entities.
- Aliases are bitemporal: `kb_recall_at` returns the alias set active at the requested
  `as_of` timestamp, consistent with memory and entity bitemporality.
- DB-level UNIQUE on `idx_alias_value_unique` makes the collision invariant enforced at
  the database, not just in application logic.
- Spreading activation and entity-anchored recall now work for memories authored under any
  alias of an entity — the `involves` edges are written to the canonical entity regardless
  of which alias token matched at write time.
- `EntityFull.aliases` exposes the alias set to callers; no hidden state.

### Negative

- Migration 021 adds one table, two indexes, and one analyzer. Migration is additive;
  no existing rows are modified.
- Touchpoints are broad: `src/types/entity.rs` (3 structs), `src/db/entity.rs`
  (upsert, get_by_name, find), `src/db/graph.rs` (link_entity × 2),
  `src/db/search.rs` (person filter), `src/db/auto_link.rs` (match_known_entities),
  `src/tools/entity.rs` (handler).
- One additional FTS lookup per upsert (collision check) and per recall query (alias
  expansion). Both are bounded by the alias index size, which is a small fraction of the
  memory count.
- `kb_entity_upsert` gains a new optional parameter. Existing callers without `aliases`
  are unaffected.

### Trade-offs accepted

- The alias namespace is global (not per-author). Two users could not assign the same
  alias string to different entities. This is acceptable for the current single-DB
  shared-namespace deployment; per-author scoping is an explicit out-of-scope item.
- `blank` tokenizer does not split hyphenated names ("Mary-Jane" is one token). Callers
  must add both the hyphenated and space-separated forms as aliases if both are needed.
  This is the correct default: aggressive tokenization causes more false positives than
  it prevents at personal-corpus scale.

## Alternatives considered

- **`aliases: array<string>` on entity** — rejected. No per-element DB-level UNIQUE
  (TOCTOU race in collision check), no per-element timestamps (bitemporal gap), BM25
  distortion on array-aggregate FTS, short-token false positives without per-element
  length enforcement.
- **SurrealDB `mapper(path)` synonym filter** — rejected. Dictionary is static and
  hardcoded at analyzer-definition time; no multi-canonical resolution (one alias must
  map to exactly one token, not to an entity record); behavior on multi-token values
  unconfirmed in v3 documentation.
- **Phonetic matching (Soundex / Metaphone)** — rejected for v1. "Bob" and "Robert"
  have edit distance 5; phonetic codes differ (`B100` vs `R163`). False-positive rate
  at personal-corpus scale (10k–100k memories) is unacceptable without a labeled query
  set to calibrate a threshold. Deferred, not permanently closed.

## References

- [ADR-014](014-anchor-extraction.md) — anchor entity extraction; alias lookup extends the match surface for anchor writes
- [ADR-022](022-adaptive-reranker-policy.md) — per-call override pattern referenced for alias expansion as an optional pre-stage
- [ADR-023](023-weighted-spreading-activation.md) — spreading activation gains correct `involves` edges once alias-matched memories are linked to canonical entities
- [ADR-029](029-spreading-activation-structural-hardening.md) — archived_at gate and hub-cap in `search.rs` person-filter; alias work depends on both pre-fixes

**Code references:**

- `src/db/migrations.rs:166,174,232` — current entity schema; Migration 021 appends to this sequence
- `src/db/auto_link.rs:106` — explicit comment "Alex will not match Alexander"; alias FTS resolves this
- Pre-fix commits: auto_link canonical anchor write, search.rs archived_at gate, search.rs involves_hub_cap
