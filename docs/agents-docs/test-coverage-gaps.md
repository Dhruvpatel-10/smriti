# Test Coverage Gap Analysis

_Generated 2026-04-10. Updated 2026-04-20. Covers all files under `tests/` and all `#[cfg(test)]` blocks in `src/`._

---

## Coverage Rubric

When evaluating test coverage, check:
1. Every new tool or DB function has at least one happy-path test and one error-path test.
2. Visibility enforcement: private/sensitive memories must be tested for hiding/showing in each query path.
3. Threshold values (dedup, auto-link, importance floors/ceilings, TTL) tested at boundary values, not just typical values.
4. Background paths (chunking, auto-link, reembed) tested at integration level (DB state verified, not just function called).
5. Export/import roundtrip tested end-to-end, not just export existence.

---

## Summary (as of 2026-04-30, 314+ tests — see note)

_Note: ADR-026 added tests for consolidation_ledger, auto-consolidation unit paths, and
un-ignored the heuristic-2 graph-cluster test (previously ignored pending array::group fix).
5 sidecar integration tests remain ignored (require live Claude API). Exact count not
re-tallied; prior total was 314 (ADR-011). Count in CLAUDE.md updated to reflect uncertainty._

| Area | Status |
|---|---|
| Memory CRUD | CLOSED 2026-04-20 |
| TTL enforcement (all 5 subtypes) | CLOSED 2026-04-20 |
| Importance floors/ceilings (all subtypes) | CLOSED 2026-04-20 |
| Search / recall | CLOSED 2026-04-20 |
| Supersession | CLOSED 2026-04-20 |
| Visibility enforcement | CLOSED 2026-04-20 |
| Auto-link thresholds | CLOSED 2026-04-20 — tests/auto_link_thresholds.rs |
| Entity matching | CLOSED 2026-04-20 |
| Dedup reject/warn thresholds | CLOSED 2026-04-20 — tests/dedup.rs |
| Chunking integration | CLOSED 2026-04-20 — tests/chunking_integration.rs |
| Export/import roundtrip | CLOSED 2026-04-20 |
| Consolidation candidates | CLOSED 2026-04-20 — tests/consolidation_candidates.rs |
| Markdown regeneration | CLOSED 2026-04-20 — tests/markdown_regen.rs |
| Reembed | CLOSED 2026-04-20 — tests/reembed.rs |
| Stale contact alert | CLOSED 2026-04-20 |
| MCP tools (17 total) | CLOSED 2026-04-20 |
| Bitemporal model (retract, BITEMPORAL_ACTIVE, recall_at fix) | CLOSED 2026-04-21 — tests/bitemporal.rs |
| Validation (unit) — unicode boundary | Open (medium) |
| SM-2 spaced repetition ease_factor increase | Open (low) |

---

## Closed Findings

### [CLOSED via ADR-011] Bitemporal model: retract, BITEMPORAL_ACTIVE predicate, recall_at correctness
**Closed 2026-04-21 — 16 tests in tests/bitemporal.rs** covering: `kb_retract` happy path and invalid ID, `retracts` edge creation with `correction_id`, BITEMPORAL_ACTIVE hides expired and retracted memories from recall, superseded memories not returned after supersession, recall_at 4-clause predicate (event_at, valid_until, created_at, invalidated_at all checked against as_of), invalidation_reason set correctly for each code path (Superseded/Retracted/Expired/Forgotten/Consolidated).

### [CLOSED via ADR-009] JSON-escape fuzzer: error responses not validated for JSON injection
**Closed 2026-04-21 — ADR-009 Fix 1: Tests added covering injection of `"`, `\n`, `\\` into error paths. All tool handlers use `serde_json::json!({...}).to_string()` for escaping. Test: tests/tool_tests.rs (json-escape fuzzer coverage).**

### [CLOSED via ADR-009] Consolidate transaction atomicity: no rollback test
**Closed 2026-04-21 — ADR-009 Fix 2: consolidate-txn-rollback test simulates mid-consolidate interruption; verifies DB state is all-or-nothing. Also: consolidate-ids-cap test verifies rejection when `ids.len() > 50`. Tests: tests/consolidation.rs.**

### [CLOSED via ADR-009] stdio-drain: post-create hooks silently abandoned on disconnect
**Closed 2026-04-21 — ADR-009 Fix 3: stdio-drain test verifies post-create hooks complete under bounded-drain shutdown. JoinSet now ensures tasks either complete or are logged before returning. Test: tests/tool_tests.rs (stdio-drain).**

### [CLOSED via ADR-009] drill-visibility: kb_drill has no visibility filter
**Closed 2026-04-21 — ADR-009 Fix 4: kb_drill now enforces visibility semantics matching recall. drill-visibility test confirms sensitive memory in project B is not returned from project A context. Test: tests/visibility.rs (drill_excludes_sensitive_without_project).**

### [CLOSED 2026-04-20] Dedup reject and warn thresholds never triggered
**Closed 2026-04-20 — tests/dedup.rs covers this.** 14 tests covering reject (>0.92), warn (>0.82), clean paths, and exact boundary values. Verifies that `DedupResult::Duplicate` causes `remember::execute` to reject the write, and `DedupResult::Warning` produces the `"Memory stored (warning: ...)"` message.

### [CLOSED 2026-04-20] `kb_consolidation_candidates` tool has zero tests
**Closed 2026-04-20 — tests/consolidation_candidates.rs covers this.** 3 tests: project+subtype cluster detected (≥3 members), graph cluster detected (≥2 relates edges), empty DB returns empty list.

### [CLOSED 2026-04-20] `kb_reembed` tool has zero tests
**Closed 2026-04-20 — tests/reembed.rs covers this.** 7 tests: batch pagination with LIMIT+OFFSET, empty/missing summary handling (failed counter incremented), loop termination on last page. ORDER BY `created_at` production bug found and fixed during testing.

### [CLOSED 2026-04-20] `kb_regenerate_md` tool has zero tests
**Closed 2026-04-20 — tests/markdown_regen.rs covers this.** 12 tests: markdown output format, visibility filter on all sections, file write, parent directory creation. ORDER BY production bug (fields missing from SELECT list) found and fixed during testing.

### [CLOSED 2026-04-20] Chunking integration not tested (chunk_and_embed)
**Closed 2026-04-20 — tests/chunking_integration.rs covers this.** Tests: chunk sub-memories created in DB for content ≥ 2000 chars, `extends` edges link each chunk back to parent, chunk memories inherit project/domain/tags from parent.

### [CLOSED 2026-04-20] Auto-link similarity thresholds not tested
**Closed 2026-04-20 — tests/auto_link_thresholds.rs covers this.** 4 integration tests: same-project `Extends` (≥0.90), same-project `Relates` (0.75–0.90), cross-project no-edge (<0.85), cross-project `Relates` (≥0.85). Memories inserted with manually-set vectors; graph edges verified after `auto_link_similar`.

### [CLOSED 2026-04-20] Visibility enforcement never tested in queries
**Closed 2026-04-20 — 3 new tests in tests/visibility.rs** covering private/sensitive hiding and project-scoped visibility. Verifies both branches of the visibility contract in `search::recall`.

### [CLOSED 2026-04-20] Supersession of non-active target has no test
**Closed 2026-04-20 — 5 new tests in tests/tool_tests.rs** covering archived/superseded target paths. The `supersede_nonexistent_returns_ok_known_behavior` test pins the silent-success behavior as a documented known discrepancy (memory::supersede does not use RETURN BEFORE; pre-check in remember.rs makes this safe in practice).

### [CLOSED 2026-04-20] Import roundtrip not tested
**Closed 2026-04-20 — 24 tests in tests/export_import.rs** covering full roundtrip, corrupt lines, unknown entry_type, duplicate ID on import. Also found and fixed a production bug in `src/db/export.rs::import_from`.

### [CLOSED 2026-04-20] Signal (14d) and Hypothesis (7d) TTLs not tested at DB layer
**Closed 2026-04-20 — `working_signal_gets_14d_ttl` and `working_hypothesis_gets_7d_ttl` tests added to tests/memory_crud.rs.**

### [CLOSED 2026-04-20] Importance floors/ceilings not tested via `remember::execute` for all subtypes
**Closed 2026-04-20 — Four tests added via `remember::execute` for LifeGoal (→5), Goal (→4), DailyLog (→2), ActionItem (→3).**

### [CLOSED 2026-04-20] `word_boundary_match` edge cases not tested
**Closed 2026-04-20 — 5 unit tests added in `#[cfg(test)]` block in src/db/auto_link.rs.** Covers: name at start/end, name embedded in longer word (no-match), short name skipped (<4 chars), name adjacent to non-ASCII character.

### [CLOSED 2026-04-20] `AlertType::StaleContact` never triggered in any test
**Closed 2026-04-20 — 7 tests in tests/alerts_and_status.rs** covering stale contact detection and recent contact suppression.

### [CLOSED 2026-04-20] `recall_at` with visibility not tested
**Closed 2026-04-20 — 2 tests in tests/visibility.rs** covering `recall_at` visibility enforcement: `recall_at_excludes_sensitive_without_project` and `recall_at_allows_private_with_project_filter`.

### [CLOSED 2026-04-20] BM25 + vector hybrid fusion path never tested end-to-end
**Closed 2026-04-20 — rrf_fuse unit tests added in src/db/search.rs `#[cfg(test)]`.** Covers: item in both lists scores higher, item only in vector list included, limit respected.

### [CLOSED 2026-04-20] Export with `exclude_sensitive=true` not verified
**Closed 2026-04-20 — `export_excludes_sensitive_memories_when_flag_set` test added to tests/export_import.rs.**

### [DOCUMENTED] `supersede` on non-existent ID silently succeeds
**Documented 2026-04-20 — `supersede_nonexistent_returns_ok_known_behavior` test pins this behavior.** `memory::supersede` does not use `RETURN BEFORE`. On a non-existent ID it returns `Ok(())`. Current call sites in `remember.rs` always pre-check via `memory::get()` so this has no practical impact. Fixing to return `NotFound` is a future enhancement.

### [CLOSED via ADR-026] Heuristic-2 graph-cluster test (was ignored pending array::group fix)
**Closed 2026-04-30** — Heuristic-2 was rewritten to use explicit graph traversal instead of
`array::group` (SurrealKV incompatibility). The previously-ignored integration test is now
un-ignored and passes.

### [CLOSED via ADR-026] `consolidation_ledger` module has no tests
**Closed 2026-04-30** — Tests in `tests/consolidation.rs` cover: attempt upsert idempotency,
skip-until fence correctly defers a cluster, outcome enum roundtrip, `merged_into` FK set on
`Merged` outcome.

### [CLOSED via ADR-026] Auto-consolidation loop has no unit tests
**Closed 2026-04-30** — Pure unit tests (no live API) cover: loop skips clusters within
cooldown, `max_per_tick` limit respected, `Sensitive` source rejection before sidecar call,
`DeferredLowConfidence` outcome when confidence < `min_confidence`. 5 sidecar integration
tests (require live Claude API + sidecar built) remain `#[ignore]`.

---

## Remaining Open Gaps

_As of 2026-04-30. All critical/high/medium gaps are closed. Items below are low priority._

### [medium] `summary` unicode-only content near 10-char boundary
`validate_summary` checks byte length (`trimmed.len()`), not char length. A 10-byte string of multi-byte UTF-8 characters (e.g., 5× "あ" = 15 bytes) passes the length check but is fewer than 10 characters. The `is_alphanumeric()` check also passes for unicode. No test verifies this boundary. No fix implemented — documenting the byte-vs-char discrepancy.

### [medium] Consolidate with 1 or 2 IDs not tested
Empty-list and 3-ID cases are tested. The 1-ID degenerate case is not; `db::consolidation` and `tools::consolidate` may or may not handle it gracefully.

### [low] `until` filter in recall never tested
`search::recall` accepts an `until` upper bound on `event_at`. No test provides a non-nil `until` value.

### [low] `recall_at` domain filter not tested
`recall_at` accepts a `domain` filter but no test passes a non-nil domain.

### [low] `link_nonexistent_memories` not tested
`kb_link` happy path only. No test calls with one or both IDs pointing to non-existent memories.

### [low] SM-2 ease_factor increase on successful recall not tested
`compute_sm2` increases `ease_factor` on every access. No end-to-end test calls `search::recall`, waits for the background SM-2 update, and verifies field updates.

### [low] `Importance::new(0)` edge case not tested
`importance=0` triggers `InvalidInput` before floor enforcement. Only `importance=6` is tested as invalid.

### [low] `kb_entity_find` with `kind=person` not tested via tool layer
Entity find tests exist at DB layer but the `kb_entity_find` server path with `kind=person` filter has no tool-layer test. (`kb_people` was dropped in ADR-024; this gap applies to its replacement.)

### [low] Spreading activation `involves`-via-entity path not tested
The `extends` edge traversal in spreading activation is tested. The `involves`-via-entity path (two memories linked through a shared entity) has no spreading activation test.
