# ADR-011: Bitemporal Memory Model (Phase 1)

**Status:** Accepted  
**Date:** 2026-04-21  
**Context:** Graphiti/Zep ship bitemporal edges (4 timestamps: t_valid, t_invalid, t_created, t_expired) — enables correct "as-of" queries and preserves correction audit trail. Smriti has `event_at` (~t_valid) and `created_at` (~t_created) on memory, but nothing for t_invalid (when a fact became false) or t_expired (when we stopped believing it). `status=archived` collapses both into a boolean with no timestamp and no reason. Current `kb_recall_at` has a correctness bug: it filters by `event_at` only, so it can return facts that were superseded long before the requested as-of date.

---

## Decision

Add three nullable fields to `memory` for bitemporal tracking (Phase 1). Skip edge-level timestamps (Phase 2) — deferred until a concrete temporal-graph use case surfaces.

---

## Why Not Phase 2 (Edge Timestamps)

Phase 2 was evaluated independently by a 4-agent expert panel on 2026-04-21. All four converged on rejection. Summary below; full panel record in Appendix A.

### Architectural mismatch (primary reason)
Graphiti's 4-timestamp edge model exists because in Graphiti **edges ARE the facts** — an `EntityEdge` is a subject-predicate-object triple, and its validity is intrinsic to the edge. Smriti's architecture is different: **memory rows are the facts**, edges are structural connectors between rows. An `involves(memory→entity)` edge inherits its validity from the memory it points to; adding independent edge timestamps denormalizes data that the memory row already owns.

This is confirmed by surveying how native bitemporal databases model relationships:
- **XTDB:** row-level bitemporality only, no special "edge" concept (relationships are rows in join tables that inherit bitemporal semantics)
- **Datomic:** bitemporality on datoms (fact assertions), which matches Graphiti's edges-as-facts model — not smriti's rows-as-facts model
- **TerminusDB:** reifies temporal relationships (edge → node with timestamps) because RDF triples can't carry properties — not a constraint smriti has
- **Academic TKG literature:** canonical quadruple `(h, r, t, τ)` uses point timestamps, not bitemporal intervals. No published benchmark demonstrates edge-bitemporal beats row-bitemporal for retrieval quality in agent memory systems.

### No query Phase 1 cannot answer
Every candidate query — "who was Alice's colleague in June 2024", "when did Alice→Bob collaboration start", "has Alice's role changed since last year" — resolves against memory rows with `event_at`/`valid_until` plus the `involves` edge as an index accelerator. No enumerable query requires independent edge timestamps.

### Concrete costs if implemented
- **Write amplification:** auto-link fires per `kb_remember` with up to 8 similarity + 64 entity candidates. 4 new fields per edge × ~16 edges/memory × 100k memories = 400–800 k extra field writes.
- **Index burden:** non-scanning edge-time predicates require composite indexes `(in, t_valid, t_invalid)` and `(out, t_valid, t_invalid)` across 6 edge tables = ~24 new B+tree structures.
- **Spreading-activation p99:** `src/db/search.rs` issues 7 edge-direction traversals per recall. Per-hop time predicate without a supporting index forces subset scans of the edge table.
- **Migration hazard:** backfill on a live graph runs synchronously at startup (`src/db/migrations.rs`), scanning 400 k+ edges. Minutes of unavailability per deploy.
- **Consolidation semantics:** merging 5 memories with 5 `involves(Alice)` edges into 1 — which `t_valid` survives? min? max? multi-edge? No defensible default.
- **LLM UX:** most edges (`extends`, `derives`, `relates`, `updates`, `authored_by`) have no meaningful `t_valid` — defaulting to `now()` is a lie; defaulting to source memory's `event_at` leaks row semantics onto the edge.

### Edge time-boundedness by type (from code survey)

| Edge type | Character | t_valid meaningful? |
|---|---|---|
| `updates` | Timeless supersession audit | No |
| `derives` | Timeless consolidation audit | No |
| `extends` (chunk) | Timeless structural link | No |
| `extends` (auto-link) | Similarity snapshot at write time | No |
| `relates` | Similarity snapshot at write time | No |
| `assigned` | Timeless ownership | No |
| `authored_by` (proposed) | Timeless authorship | No |
| `involves` | Partially time-bounded (Person/Org lifetimes) | Marginal — already partially expressed via `entity.attrs.last_contact` and the memory's own `event_at`/`valid_until` |

Only `involves` has any temporal character, and the `entity.attrs.last_contact` side-effect plus memory-level timestamps already cover the real query cases.

### Fallback options if a real use case emerges later

If a concrete query surfaces that Phase 1 provably cannot answer, adopt the cheapest sufficient option in this order:

| Option | Cost | When to use |
|---|---|---|
| **Derive-on-read helper** | ~2 hours | Query needs edge-validity at read time — compute from endpoint memories in a helper function, no schema change |
| **Entity-level `valid_from`/`valid_until`** | ~4 hours | Person/Org lifecycle queries ("Alice was at Acme from X to Y") — one field per entity, not per edge |
| **Selective `involves`-only edge timestamps** | ~1 day | Only if the above two cannot express the query AND it recurs frequently enough to justify write-path + spreading-activation cost |
| **Blanket Phase 2** | Rejected | Architectural mismatch; no benefit over Phase 1 + the above options |

### Bar to reconsider
Phase 2 will only be reconsidered with a documented list of **5+ concrete queries** that (a) Phase 1 memory timestamps provably cannot answer, (b) entity-level timestamps cannot answer, (c) derive-on-read cannot answer, and (d) recur often enough to justify the write-path, index, and migration cost. No such list exists at the time of this ADR.

---

## Schema (Migration 007)

```sql
DEFINE FIELD valid_until          ON memory TYPE option<datetime>;
DEFINE FIELD invalidated_at       ON memory TYPE option<datetime>;
DEFINE FIELD invalidation_reason  ON memory TYPE option<string>;

DEFINE INDEX idx_memory_valid_until    ON memory FIELDS valid_until;
DEFINE INDEX idx_memory_invalidated_at ON memory FIELDS invalidated_at;
```

---

## Field Mapping to Graphiti Model

| Graphiti | smriti | Notes |
|---|---|---|
| t_valid | `event_at` (existing) | When fact became true |
| t_invalid | `valid_until` (new, nullable) | When fact became false; NULL = still true |
| t_created | `created_at` (existing) | When we recorded it |
| t_expired | `invalidated_at` (new, nullable) | When we unlearned / superseded it |

---

## InvalidationReason Enum

```rust
pub enum InvalidationReason {
    Superseded,   // replaced by newer memory via updates_id
    Retracted,    // user explicitly marked wrong
    Expired,      // TTL reached (working memory)
    Forgotten,    // user kb_forget — no longer relevant, not necessarily wrong
    Consolidated, // merged into another memory via kb_consolidate
}
```

---

## Relationship to status Field

`invalidated_at` becomes the authoritative source of truth. `status` is retained as a derived index hint for query performance.

- **Migration rule:** `status=archived` iff `invalidated_at IS NOT NULL`.
- **Write rule:** setting `invalidated_at` also sets `status=archived`.

---

## Insertion Changes

### Supersession (updates_id=X)

```sql
-- Before
CREATE new_memory;
UPDATE X SET status = 'archived';
RELATE new -[updates]-> X;

-- After
CREATE new_memory WITH valid_from = $event_at, valid_until = NULL;
UPDATE X SET
  valid_until          = $event_at,
  invalidated_at       = time::now(),
  invalidation_reason  = 'superseded',
  status               = 'archived';
RELATE new -[updates]-> X;
```

### TTL Expiry (working memory prune)

```sql
UPDATE memory SET
  invalidated_at      = time::now(),
  invalidation_reason = 'expired',
  status              = 'archived'
WHERE ttl_expires_at < time::now()
  AND invalidated_at IS NULL;
```

### kb_forget

```sql
UPDATE $id SET
  invalidated_at      = time::now(),
  invalidation_reason = 'forgotten',
  status              = 'archived';
```

---

## New Tool: kb_retract

`kb_retract(id, correction_id?)` — explicit "I was wrong" semantics, distinct from `kb_forget` ("no longer relevant"). Sets `invalidation_reason=retracted`. If `correction_id` is provided, creates a `RELATE correction -[retracts]-> target` edge for audit trail.

> **Amendment (2026-04):** Original ADR proposed a free-form `reason?` string parameter. Implementation dropped it — `invalidation_reason` is the structured `Retracted` enum value, and the audit trail comes from the `correction_id` edge rather than prose. A retraction note can still be authored as a separate memory and linked via `correction_id`. Idempotency: a second retract on an already-invalidated target returns `InvalidInput("already invalidated")` without producing a spurious `retracts` edge.

---

## Retrieval Changes

### Default recall predicate

```sql
-- Before
WHERE status = 'active'

-- After
WHERE invalidated_at IS NULL
  AND (valid_until IS NULL OR valid_until > time::now())
```

### kb_recall_at (correctness fix)

```sql
-- Before: only filtered by event_at — returned superseded facts
-- After
WHERE event_at <= $as_of
  AND (valid_until IS NULL OR valid_until > $as_of)
  AND created_at  <= $as_of
  AND (invalidated_at IS NULL OR invalidated_at > $as_of)
```

### New query modes

| Mode | Predicate |
|---|---|
| "What did I used to believe?" | `WHERE invalidated_at IS NOT NULL ORDER BY invalidated_at DESC` |
| Knowledge-cutoff view | `kb_recall(knowledge_cutoff=as_of)` — full bitemporal as-of |

---

## Consolidation Provenance

Merged memory inherits:
- `event_at = min(source event_ats)`
- `valid_until = max(source valid_untils)` if all sources have `valid_until` set; otherwise NULL

---

## Interaction with SM-2 Decay

- Memories with `valid_until` in the past decay faster — historical facts, not active beliefs.
- Memories with `valid_until=NULL` decay at the normal SM-2 rate.
- Stale facts fall out of top-k naturally without requiring explicit retraction.

---

## Interaction with Agent Model (ADR-010)

- `agent.retired_at` is the per-agent analog of `invalidated_at` — same pattern, orthogonal axis.
- A memory authored by a retired agent retains its own `invalidated_at=NULL` if the fact still holds; agent retirement and fact invalidation are independent.
- `InvalidationReason` enum is extensible if the two models converge — not needed now.

---

## LLM Caller UX

| Field | Default | Who sets it |
|---|---|---|
| `valid_until` | NULL (still true) | Caller, rarely — only if termination date is known at write time |
| `invalidated_at` | NULL | System only (supersession, retraction, TTL, forget) |
| `invalidation_reason` | NULL | System only |

Zero additional classification burden on the caller.

---

## Query Sites Updated

`search.rs` (recall, recall_at, status query), `alerts.rs`, `export.rs`, `prune.rs`, `consolidation.rs` — approximately 8 sites.

---

## Consequences

### Positive
- Ghost-fact elimination: stale facts stop polluting recall indefinitely.
- Correction audit preserved: chronological belief evolution is queryable.
- `kb_recall_at` correctness bug fixed.
- Self-reflection queries unlocked ("what did I used to believe about X").
- Consolidation provenance gains valid-time bounds.

### Negative
- 3 new nullable columns on `memory` (~16 bytes each when non-null).
- ~8 query sites need predicate updates.
- `status` field is now derived — must enforce the invariant at every write path or risk drift.

### Trade-offs accepted
- `valid_until` defaults to NULL (still true) — callers rarely know the termination date at write time, so most records will carry only `invalidated_at` as the expiry signal.
- Automatic conflict detection (writing "X" when "not X" is still valid) is not implemented — surfaced via `kb_alerts` for manual reconciliation.

---

## Out of Scope

- Phase 2 edge-level bitemporality.
- Automatic conflict detection.
- Cross-user belief reconciliation (single-user invariant preserved).

---

## Implementation

**Shipped:** 2026-04-21

**Migrations:**
- Migration 007 — adds `valid_until`, `invalidated_at`, `invalidation_reason` fields + indexes to `memory`
- Migration 008 — adds `retracts` edge table

**Files changed:**
- `src/types/enums.rs` — `InvalidationReason` enum (Superseded, Retracted, Expired, Forgotten, Consolidated)
- `src/types/memory.rs` — 3 new nullable fields: `valid_until`, `invalidated_at`, `invalidation_reason`
- `src/db/mod.rs` — `BITEMPORAL_ACTIVE` constant (`invalidated_at IS NONE AND (valid_until IS NONE OR valid_until > time::now())`)
- `src/db/memory.rs` — archive/supersede/retract write paths set bitemporal fields
- `src/db/search.rs` — all recall/recall_at query sites swept to `BITEMPORAL_ACTIVE`; recall_at 4-clause correctness fix
- `src/db/consolidation.rs` — provenance inherits `event_at = min(source)`, `valid_until = max(source)` or NULL
- `src/db/prune.rs` — TTL expiry sets `invalidation_reason = expired`
- `src/tools/retract.rs` — new tool implementing `kb_retract`
- `src/server.rs` — `kb_retract` registered

**Tests:** 16 integration tests in `tests/bitemporal.rs`.

**Known Phase 2 follow-ups:**
- SM-2 decay does not yet accelerate for memories with past `valid_until` (Phase 2)
- `kb_remember` does not yet expose `valid_until` to callers (Phase 2)
- `retracts` edge has no `created_at` field — audit order via correction memory's `created_at`
- archive→retract sequencing blocked by idempotency guard (product decision pending)

---

## Related

- ADR-010: Agent authorship — `agent.retired_at` follows the same soft-invalidation pattern
- ADR-006: SM-2 decay — decay rates interact with `valid_until` as described above
- ADR-005: Hybrid retrieval — recall predicate updated at all 8 query sites

---

## Appendix A: Phase 2 Expert Panel (2026-04-21)

Four independent agents evaluated blanket Phase 2 (4 timestamps on every edge). All four recommended rejection.

### Researcher (web + Graphiti source + temporal-KG literature)
- Confirmed Graphiti's 4-timestamp model applies **only to `EntityEdge`** (semantic edges). Episodic/community/structural edges carry `created_at` only. Source: `graphiti_core/edges.py`, arXiv 2501.13956 §2.2.3.
- Surveyed XTDB, Datomic, TerminusDB, and academic TKG literature: no system applies bitemporal intervals to structural edges in a row-as-fact model. Row-level bitemporality is the canonical pattern when facts are rows.
- Verdict: "Phase 2 is Graphiti-copycat for smriti's architecture. Memory-level bitemporality is sufficient."

### Devils-advocate (red-team)
- Attack: derivable data, duplicated. Edge validity = `max(src.valid_from, dst.valid_from)` to `min(src.valid_until, dst.valid_until)` for 5 of 6 relations — denormalizing a computed field.
- Attack: no query justifies it. Challenged to name one Phase 1 cannot answer; none produced.
- Attack: write blast radius quadratic in hooks; consolidation timestamp semantics a minefield; migration is a one-way door.
- Verdict: "Kill blanket Phase 2. Ship selective on `involves` only if a real query emerges. Bar to flip: 5+ concrete queries Phase 1 provably cannot answer."

### Auditor (concurrency + perf)
- Concrete impact: ~24 new composite B+tree indexes to keep spreading-activation non-scanning; per-hop time predicate across 7 edge directions doubles P99; synchronous startup backfill on 400 k+ edges causes minutes of downtime per deploy.
- RELATE UNIQUE conflict behavior: first-writer-wins silently drops loser's timestamps — surprising audit characteristic.
- Verdict: "Reaffirm ADR-011 memory-level design. Edge-level adds ~24 indexes + migration risk + spreading-activation regression for no enumerable query gain."

### Code-explorer (edge surface map)
- Confirmed 7 active edge types; 3 RELATE creation sites (`src/db/graph.rs:34,79,121`); 8 traversal sites in `src/db/search.rs:159-165` and `src/db/consolidation.rs:87`.
- Time-boundedness classification: 6 of 7 edge types are timeless audit or similarity snapshots; only `involves` has marginal time semantics, already partially covered by `entity.attrs.last_contact` side-effect (graph.rs:61, 108).
- `belongs` edge type declared but unused.

### Convergence
All four agents independently arrived at the same conclusion via different analysis paths (literature review, attack enumeration, perf measurement, code survey). No dissenting opinion on the core architectural point: smriti stores facts as rows, so temporal data belongs on rows, not edges.

---

## Update 2026-04-29 — recall_at default = valid-time slice

**Decision:** `kb_recall_at` uses a valid-time predicate only. The `(invalidated_at IS NONE OR invalidated_at > $as_of)` transaction-time clause has been dropped from both the BM25 and vector arms in `src/db/search.rs`.

**Rationale:** Domain-history queries ("what did I prefer in May 2024?") ask about what was factually true at a point in valid-time, not about the transaction-time audit trail of belief changes. Filtering by `invalidated_at` caused a recall regression: a fact that was valid in May 2024 but later superseded (setting `invalidated_at = now()`) would be invisible to `kb_recall_at(as_of=May-2024)` — which is wrong. This aligns with XTDB's valid-time vs. system-time query modes, Fowler's bitemporal model, and Zep's temporal-graph design where valid-time and transaction-time are orthogonal axes.

**Impact:** `kb_recall()` (current-time path) is unchanged — it continues to use `BITEMPORAL_ACTIVE` which correctly intersects both axes for present-state queries. `BITEMPORAL_ACTIVE_NO_TTL` used by prune is also unchanged.

**Future:** An explicit `mode="tx"` flag on `kb_recall_at` may be added if an audit use case (e.g. "show me everything I believed at transaction-time T") emerges. Out of scope now.

---

## Update 2026-05-01 — exclude_retracted filter on recall_at (correctness fix)

**Problem:** Dropping the `invalidated_at` transaction-time clause (2026-04-29 update) was correct for superseded facts ("the data changed") but created a correctness defect for retracted facts ("I was wrong"). A memory retracted via `kb_archive(reason=retract)` carries `invalidation_reason = 'retracted'`, meaning the user explicitly disowned the information. Returning it from `recall_at` caused any LLM reading historical context to receive information the user disowned — a qualitatively different failure from showing a superseded fact.

**Decision:** Add `exclude_retracted: bool` parameter to `search::recall_at` and `kb_recall_at` MCP tool input. Default = `true` (safe: hide retracted memories). Callers pass `false` for forensic/audit work where seeing disowned facts is intentional.

**Filter predicate added (both BM25 and vector arms when exclude_retracted=true):**

```sql
AND (invalidated_at IS NONE OR invalidation_reason != $retracted_reason)
```

`$retracted_reason` is bound as `InvalidationReason::Retracted.as_str()` — never a raw string literal in Rust.

**Invariant preserved:** Only `InvalidationReason::Retracted` is filtered. Superseded, forgotten, consolidated, and expired memories are still returned when their valid-time window covers `as_of`. This maintains the 2026-04-29 design for non-retraction invalidation paths.

**MCP tool change:** `RecallAtInput.exclude_retracted: Option<bool>` — `None` resolves to `true`. Callers that do not send the field get safe-by-default behavior with no breaking change.

**Files changed:**
- `src/db/search.rs` — `recall_at` gains `exclude_retracted: bool` param; both BM25 and vector predicates updated; comment block updated
- `src/tools/recall_at.rs` — `RecallAtInput` gains `exclude_retracted: Option<bool>`; `execute` resolves default and threads through
- `tests/bitemporal.rs` — 3 new tests: `recall_at_default_excludes_retracted_within_valid_window`, `recall_at_exclude_retracted_false_returns_retracted_in_valid_window`, `recall_at_default_does_not_filter_superseded_memories`
