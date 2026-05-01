# SurrealDB Query Audit

**Date:** 2026-04-10
**Scope:** All `.rs` files under `src/db/` and `src/tools/` containing SurrealQL queries.

---

## Summary

18 source files audited. 157 SurrealQL query sites identified. Parameterized bindings used consistently. RELATE idempotency handled via pre-check + unique indexes. Visibility and status filters applied correctly across all query paths. All critical, high, and medium findings closed.

**Critical: 0 | High: 3 (3 closed) | Medium: 7 (7 closed) | Low: 4 (open) | Info: 3**

---

## Query Audit Rubric

When reviewing SurrealQL queries, check:
1. All user-controlled values go through `.bind()` — no `format!` interpolation of user input.
2. `BITEMPORAL_ACTIVE` predicate (`invalidated_at IS NONE AND (valid_until IS NONE OR valid_until > time::now())`) applied at all user-facing query sites via the constant in `src/db/mod.rs`. `WHERE status = 'active'` is retained as a derived index hint but is no longer the canonical filter.
3. Visibility filter applied: cross-project → `visibility = 'standard'`; project-scoped → `visibility != 'sensitive'`.
4. No N+1 patterns: related data fetched in one query or via a batch.
5. UPDATE results verified (RETURN AFTER/BEFORE, not silently discarded).
6. ORDER BY fields present in SELECT projection (SurrealDB v3 requirement).
7. `search::score(N)` used for raw BM25 score extraction — index `N` must match the positional BM25 index defined on the table; alias as `_bm25_score` for downstream fusion. Never use `search::score()` without the index argument in v3 (ambiguous when multiple BM25 indexes exist on a table). Convex Combination fusion path surfaces this via `(search::score(0) ?? 0) AS _bm25_score`.

---

## Closed Findings

### [CLOSED] src/db/search.rs:66-79 — N+1 SM-2 update loop in spawned task
**Closed 2026-04-20 — Batched into single SurrealDB FOR loop: `"FOR $u IN $updates { UPDATE type::record('memory', $u.id) SET ease_factor = $u.ease_factor, ... }"`; eliminates the N+1 pattern and adds proper error logging.**

### [CLOSED] src/db/auto_link.rs:74-88 — N+1 project check inside similarity loop
**Closed 2026-04-20 — `find_similar` already returns `project` field in its SELECT; N+1 `check_same_project()` queries eliminated by using the already-fetched value.**

### [CLOSED] src/db/auto_link.rs:100-102 — Full table scan on entity for name matching
**Closed 2026-04-20 — `match_known_entities()` now uses `SELECT id, name FROM entity WHERE name @@ $candidate LIMIT 1` per candidate token; migration 005 adds `idx_entity_name_ft` (FULLTEXT ANALYZER, BM25); `MAX_CANDIDATES = 64` caps the candidate loop.**

### [CLOSED] src/db/memory.rs:112-120 — UPDATE without verifying result in get_and_touch
**Closed 2026-04-20 — `get_and_touch()` now uses `RETURN AFTER` and returns `NotFound` on 0 rows.**

### [CLOSED] src/db/memory.rs:146-153 — UPDATE without verifying result in supersede
**Closed 2026-04-20 — `supersede()` confirmed using `RETURN AFTER` pattern; also now uses `WHERE status = 'active'` for atomic conditional update. (ADR-011 2026-04-21 updated guard to `WHERE invalidated_at IS NONE`.)**

### [CLOSED] src/db/memory.rs:161-171 — UPDATE without verifying result in promote
**Closed 2026-04-20 — `promote()` now uses `RETURN AFTER` and returns `NotFound` on 0 rows.**

### [CLOSED] src/db/search.rs:370-380 — format! interpolation for vector query filters
**Closed 2026-04-20 — Safe by analysis; only static string literals interpolated, all user values via `.bind()`. No user input reaches the format! fragments. Noted as fragile pattern — future edits should prefer the conditions-vec approach.**

### [CLOSED] src/db/prune.rs:117-133 — format! interpolation of immunity_condition()
**Closed 2026-04-20 — `MemorySubtype::immune_strs()` method added; subtype values now bound via `.bind()` rather than interpolated; `IMPORTANCE_IMMUNE_THRESHOLD` also bound as a parameter.**

### [CLOSED] src/db/search.rs:438-439 — string::lowercase CONTAINS for person matching is not exact match
**Closed 2026-04-20 — Changed to exact case-insensitive equality (`string::lowercase(name) = string::lowercase($name)`); prevents "Al" from matching "Alice", "Albert", etc.**

### [CLOSED] src/db/consolidation.rs:81-88 — Full table scan with LIMIT 200 for graph clusters
**Closed 2026-04-20 — Query uses explicit projection and filters; verified not a full table scan.**

### [CLOSED — PRODUCTION BUG] src/db/markdown.rs — ORDER BY field missing from SELECT list
**Closed 2026-04-20 — Found and fixed during Wave H testing. SurrealDB v3 requires ORDER BY fields in SELECT projection; all four `generate_summary` queries now include the ORDER BY field (`importance`, `event_at`, `created_at`). Was causing non-deterministic CLAUDE.md section ordering.**

### [CLOSED — PRODUCTION BUG] src/db/reembed.rs — ORDER BY field missing from SELECT list
**Closed 2026-04-20 — `reembed_all()` used `ORDER BY created_at` but `created_at` was absent from SELECT. Same root cause as markdown.rs. Caused non-deterministic batch ordering in reembed pagination; fixed by adding `created_at` to the SELECT list.**

---

## Open Findings (Low priority)

### [low] src/db/reembed.rs:60-91 — N+1 embed+store in reembed loop
`reembed_all()` processes memories one at a time, issuing individual UPDATE queries per memory. Embedding is CPU-bound and sequential (model inference), so the DB update is not the bottleneck. Batching the UPDATEs would provide marginal improvement.

### [low] src/db/export.rs:29-33 — Export queries lack ORDER BY
`export_all()` queries `SELECT * FROM memory` and `SELECT * FROM entity` without ORDER BY. Export order is nondeterministic, making it harder to diff exports across runs. Consider `ORDER BY created_at ASC` for reproducibility.

### [low] src/db/search.rs:148 — record::id(id) IN $ids may not use index
In spreading activation, `WHERE record::id(id) IN $ids` applies a function to the id field before comparison, which may prevent index usage. Used in multiple places (lines 148, 215, 517). Should be verified against the query planner.

### [low] src/db/memory.rs:71-72 — serde_json::to_value for enum bindings
`memory::create()` uses `serde_json::to_value(memory_type).unwrap()` to bind enum values. Adds unnecessary allocation. Surreal SDK should accept the enum directly if it implements `Serialize`. Minor perf nit, not a correctness issue.

---

## Info Findings (no action needed)

### [info] src/db/graph.rs:32-34 — format! for edge table name is safe
`link_memories()` interpolates `{table}` into RELATE SQL from `RelationType::to_string()`, which is a Rust enum — not user input. `RelationType::Display` delegates to `as_str()` (compile-time match, infallible). Documented at line 22.

### [info] src/db/dedup.rs:38-46 — Vector binding as Vec<f32> is correct
`check_duplicate()` and `find_similar()` bind the embedding vector as `embedding.to_vec()` (Vec<f32>). Surreal Rust SDK serializes Vec<f32> to a JSON array of numbers; `vector::similarity::cosine()` accepts this. Schema defines `vector` as `option<array<float>>`.

### [info] src/db/search.rs:286-296 — Visibility filtering is consistent and correct
All recall paths correctly apply the two-branch visibility contract. `BITEMPORAL_ACTIVE` predicate applied at all query sites (ADR-011, 38 sites swept in search.rs). `WHERE status = 'active'` retained as index hint, kept in sync by write paths.

### [info] src/db/consolidation_ledger.rs — new `consolidation_attempt` table (migration 018, ADR-026)
New table introduced by auto-consolidation (ADR-026). All queries use parameterized bindings
— no string interpolation of user-controlled values. `fetch_source_metadata` (the inner
re-fetch of source summaries before the sidecar call) applies the `BITEMPORAL_ACTIVE`
predicate (`WHERE invalidated_at IS NONE`) to prevent already-archived memories from being
sent to the LLM. The `cluster_hash` UNIQUE index prevents duplicate attempt rows. No action
required — compliant with audit rubric on all points.

### [info] src/db/consolidation.rs — `find_candidates` signature and SQL changes (ADR-026)
`find_candidates` now accepts `min_age_days: Option<u32>` and `max_cluster_size: Option<u32>`
parameters, both bound as `$min_age_days` / `$max_cluster_size` via `.bind()`. The GROUP BY
clause now includes `author` for per-author scoping — prevents cross-tenant cluster formation
on the shared daemon (ADR-002). Heuristic-2 (graph-cluster detection) filters neighbor
visibility to `Standard` only (`WHERE visibility = 'standard'`), preventing `Sensitive`
memories from anchoring clusters. `fetch_source_metadata` uses `WHERE invalidated_at IS NONE`
(`BITEMPORAL_ACTIVE` predicate) before passing summaries to the sidecar. No `array::group`
usage — rewrote to use explicit graph traversal to avoid SurrealKV incompatibility (see
surrealdb-v3.md). All findings: compliant.
