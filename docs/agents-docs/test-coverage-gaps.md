# Test Coverage Gap Analysis

_Generated 2026-04-10. Covers all files under `tests/` and all `#[cfg(test)]` blocks in `src/`._

---

## Summary (covered vs gaps)

| Area | Tests exist | Key gaps |
|---|---|---|
| Memory CRUD | Solid | `supersede` not-found path, `supersede` on already-superseded |
| TTL enforcement | 3/5 subtypes tested end-to-end | Signal (14d) and Hypothesis (7d) TTLs not DB-tested |
| Importance floors/ceilings | 5/8 subtype bounds tested | LifeGoal, Goal, DailyLog floors/ceilings not tested end-to-end via `remember` |
| Validation (unit) | Good | Summary exactly-501 chars not checked; unicode-only summary edge case |
| Search / recall | Good | Cross-project visibility leak (Private + Sensitive), vector+BM25 hybrid path |
| Supersession | Happy path only | Old memory already-archived/superseded → warn+skip path untested |
| Visibility enforcement | Not tested at all | No test verifies Private/Sensitive memories are hidden in cross-project queries |
| Auto-link thresholds | Not tested | 0.75/0.85/0.90 cosine thresholds, cross-domain vs same-project logic |
| Entity matching | Not tested | `word_boundary_match` edge cases, unicode names, short-name skip (<4 chars) |
| Dedup | Not tested | Reject (>0.92) and Warn (>0.82) threshold paths |
| Chunking | Unit only | No integration test: `chunk_and_embed` creates child memories + extends edges |
| Export | Partial | Import roundtrip not verified; corrupt/partial file error paths |
| Consolidation candidates | Not tested | `find_candidates` with project+subtype clusters and graph clusters |
| Markdown regeneration | Not tested | `generate_summary` / `regenerate_file` entirely uncovered |
| Reembed | Not tested | `reembed_all` entirely uncovered |
| Stale contact alert | Not tested | `AlertType::StaleContact` never triggered in any test |
| MCP tools (17 total) | 14/17 covered | `kb_consolidation_candidates`, `kb_reembed`, `kb_regenerate_md` have zero tests |
| SM-2 spaced repetition | Reasonably covered | `compute_sm2` (internal) has no direct unit test; no test for ease_factor increase on successful recall |

---

## Covered Paths

### Memory CRUD (`tests/memory_crud.rs` + `src/db/memory.rs`)
- Create + get round-trip with all fields
- Invalid type/subtype pairing rejected
- Get non-existent returns NotFound
- `get_and_touch` increments access_count
- Archive (happy path) and archive non-existent
- Supersede (happy path)
- Promote clears TTL, bumps importance to ≥3
- Promote preserves already-high importance
- Scratch (48h), Priority (7d), OpenQuestion (30d) TTLs verified at DB layer
- Semantic/Decision has no TTL
- Count by status
- `event_at` defaults to now; explicit past `event_at` is preserved
- Connect+disconnect+reconnect data persistence

### Validation (unit, `src/db/validation.rs`)
- Summary too short (<10), too long (>500), no alphanumeric
- Summary valid, exact boundary (10 and 500 chars)
- Importance floor: Decision → floor 4, ceiling: Scratch → ceil 2, Value → fixed 5, unconstrained pass-through
- TTL: Scratch (48h), Signal (14d), non-working type → None
- Contextual text with/without project/domain

### Types (unit, `src/types/enums.rs`)
- All `Importance` valid/invalid values, default, ordering, serde
- All 23 subtypes round-trip through serde
- All valid type+subtype pairings checked
- Multiple invalid cross-type pairings checked
- MemoryStatus, Visibility, RecallScope defaults
- RelationType round-trip and display

### Search / Recall (`tests/search_retrieval.rs`)
- Empty query returns all active
- Filters: type, project, domain, time range, limit, limit cap at 25
- Excludes archived and superseded
- Multiple domains (found by any)
- Combined type+project filter
- Person filter via entity table (both match and no-match)
- Importance ordering
- Spreading activation via graph edges
- Vector field storage and cosine similarity ordering
- f32 cosine precision (identical vector → score ≈ 1.0)

### Tool handlers (`tests/tool_tests.rs`)
- `kb_remember`: basic, full fields, invalid importance, invalid subtype, people→entity links, supersession archives old
- `kb_recall`: empty, by project, by domain, limit, cap at 25, time range
- `kb_recall_at`: bi-temporal filter (future/past as_of)
- `kb_drill`: full content, access count, non-existent error
- `kb_forget`: archives + hidden in recall, non-existent error
- `kb_promote`: clears TTL, bumps importance
- `kb_link`: creates relation, all four RelationType values
- `kb_consolidate`: merges 3 memories, archives originals, creates derives edges, empty ids error
- `kb_entity_upsert/find/get`: CRUD, idempotency, fuzzy search, kind/role filter, memory_count
- `kb_status`: empty DB, counts active, action items, project filter
- `kb_alerts`: empty DB, overdue action items, future items ignored

### Consolidation (`tests/consolidation.rs`)
- `derives` edges created from consolidated → originals
- Originals archived

### Alerts (`tests/alerts_and_status.rs`)
- Overdue action items
- Contradiction/unsuperseded updates (detected and suppressed when properly superseded)
- Expiring memory within window / beyond window
- Status query: action items, decisions, open questions, total/active counts, project filter

### Pruning (`tests/pruning_and_decay.rs`)
- TTL expiry archives expired, leaves future-expiry alone, ignores no-expiry
- Access decay archives low-importance unaccessed memories
- `prune_run_all` on empty DB
- SM-2: skip not-overdue; decay overdue+unaccessed; reset review_interval on miss; ease_factor floor at 1.3; legacy path (no SM-2 fields)

### Graph/Entity (`tests/graph_and_entities.rs`)
- `link_memories` creates edges; `link_entity` upserts+links (idempotent)
- `find_entities` match/no-match
- Entity CRUD via `db::entity`
- Duplicate `RELATE` edges prevented (idempotent)
- Duplicate `involves` edges prevented

### Queue (`tests/queue_tests.rs`)
- Write entry, valid JSON structure, accumulate multiple entries
- Creates directory if missing
- Rejects when full (size limit), size limit of one
- Success message returned
- Empty file handled gracefully

### Chunking (unit, `src/db/chunking.rs`)
- Short text not chunked
- Long text produces multiple chunks
- Paragraph boundary preference

### Export (`tests/export_import.rs`)
- Export creates file with correct memory_count

### Migrations (`tests/migrations.rs`)
- Migrations idempotent; all 4 versions recorded

---

## Coverage Gaps

### [critical] Visibility enforcement never tested in queries
**What's missing:** No test creates a `Visibility::Private` or `Visibility::Sensitive` memory and then verifies it is hidden (or not) when `recall` is called with and without a `project` filter.

**Why it matters:** The visibility logic in `search::recall` has two distinct branches:
- No project filter → `visibility = 'standard'` (blocks both Private and Sensitive)
- Project filter → `visibility != 'sensitive'` (allows Private within the project)

A bug in this string-building logic (e.g., wrong condition, missing AND) would silently leak private memories across projects. This is a data-privacy correctness requirement with no test coverage at all.

**Where:** `src/db/search.rs` lines 289-295 and the equivalent branch in vector search.

---

### [critical] Supersession of non-active target is silently skipped — no test verifies this
**What's missing:** `remember.rs` lines 167-179 contain logic: if the `updates_id` target is not Active (already archived or superseded), it logs a warning and continues — the new memory is stored but no supersession occurs. No test exercises either the "already archived" or "already superseded" paths.

**Why it matters:** If a client sends `updates_id` pointing to an already-archived memory, the behavior is a silent no-op. The caller gets back `"Memory stored"` with no indication the supersession was skipped. This could leave inconsistent state undetected.

---

### [critical] Dedup reject and warn thresholds never triggered
**What's missing:** `src/db/dedup.rs` contains `REJECT_THRESHOLD = 0.92` and `WARN_THRESHOLD = 0.82` thresholds, but no test inserts two nearly-identical vectors, calls `check_duplicate`, and verifies that `DedupResult::Duplicate` is returned (and that `remember::execute` rejects the write) or that `DedupResult::Warning` produces a `"Memory stored (warning: ...)"` message.

**Why it matters:** The dedup system is one of the most important data-quality guardrails. A regression in the threshold logic (off-by-one in the `>` vs `>=` comparison, or the wrong constant) would silently allow duplicates or silently reject valid writes without any test catching it.

---

### [critical] Import roundtrip not tested
**What's missing:** `tests/export_import.rs` only calls `export_all` and checks the file exists and `memory_count == 2`. It never calls `import_from` on the resulting file and verifies the memories are restored. The error-recovery paths (corrupt line, unknown entry_type, duplicate ID on import) have no coverage at all.

**Why it matters:** An export file that cannot be imported is a silent backup failure. The `import_from` function's per-line error accumulation is non-trivial logic that could silently corrupt a restore.

---

### [high] Signal (14d) and Hypothesis (7d) TTLs not tested at DB layer
**What's missing:** `tests/memory_crud.rs` tests Scratch (48h), Priority (7d), and OpenQuestion (30d). Signal (14d) and Hypothesis (7d) TTLs are validated at unit level in `src/db/validation.rs` (`ttl_signal_14d` test) but never end-to-end through `memory::create` → `memory::get` to verify the `expires_at` field is actually written with the right duration.

**Why it matters:** The `ttl_duration()` function and `memory::create` are separate code paths. A mismatch (e.g., `create` passing the wrong expiry) would not be caught by the unit test in validation.rs.

---

### [high] Importance floors/ceilings not tested via `remember::execute` for all subtypes
**What's missing:** `src/db/validation.rs` unit tests cover Decision, Scratch, and Value floor/ceiling enforcement. But:
- `LifeGoal` (floor=5, ceiling=5)
- `Goal` (floor=4, ceiling=5)
- `DailyLog` (floor=1, ceiling=2)
- `ActionItem` (floor=3, ceiling=5)

None of these are tested through `remember::execute` to verify that enforcement actually propagates all the way through the tool layer.

**Why it matters:** The `remember::execute` code at line 71 calls `validation::enforce_importance` before `memory::create`. If this call were accidentally removed, or if a future refactor bypassed it, no integration test would catch the regression.

---

### [high] Auto-link similarity thresholds not tested
**What's missing:** `src/db/auto_link.rs` defines three thresholds (RELATES_THRESHOLD=0.75, CROSS_DOMAIN_THRESHOLD=0.85, EXTENDS_THRESHOLD=0.90) and logic that selects `Extends` vs `Relates` vs no-link based on cosine score AND same-project check. No test inserts memories with controlled vectors, calls `auto_link_similar`, and verifies:
- Score in [0.75, 0.90) + same project → `Relates` edge created
- Score ≥ 0.90 + same project → `Extends` edge created
- Score in [0.75, 0.85) + different project → no edge (below cross-domain threshold)
- Score ≥ 0.85 + different project → `Relates` edge created
- Score < 0.75 → no edge

**Why it matters:** The four-branch threshold logic is the most complex decision tree in the auto-link system. A bug changing `>=` to `>` on any threshold would silently create wrong edge types without detection.

---

### [high] `word_boundary_match` edge cases not tested
**What's missing:** `src/db/auto_link.rs` has `word_boundary_match` which is only used internally (not exported) and has no `#[cfg(test)]` block. The cases not covered:
- Name at the start of a string (left boundary = index 0)
- Name at the end of a string (right boundary = len)
- Name embedded inside a longer word (should NOT match — e.g., "Jane" in "Plaintext")
- Short names skipped (< 4 chars) — the `name.len() < 4` guard
- Unicode entity names (the byte-based boundary check uses `is_ascii_alphanumeric`, which is correct for ASCII but could miss unicode word boundaries — e.g., a name preceded by a non-ASCII accented character)

**Why it matters:** The byte-index arithmetic in `word_boundary_match` (line 135-148) could panic on non-ASCII input if a multi-byte UTF-8 character boundary is not correctly handled (the code uses `haystack_bytes[i - 1]` where `i` is a char boundary from `match_indices`, but `i - 1` could be mid-character for non-ASCII left-adjacent chars). This is a potential panic path.

---

### [high] `kb_consolidation_candidates` tool has zero tests
**What's missing:** `src/db/consolidation.rs::find_candidates` and the corresponding tool call from `server.rs` are completely untested. Neither heuristic (project+subtype cluster with ≥3 members, or graph cluster with ≥2 `relates` edges) is exercised.

**Why it matters:** This is one of the 17 listed MCP tools. The GROUP BY + HAVING ≥3 SurrealQL query is non-trivial and untested.

---

### [high] `kb_reembed` tool has zero tests
**What's missing:** `src/db/reembed.rs::reembed_all` is entirely uncovered. No test verifies batch pagination (LIMIT + START OFFSET), the handling of empty/missing summaries (failed counter), or that the function terminates correctly on the last page.

**Why it matters:** The off-by-one in offset accumulation (`offset += batch_count`) or a loop termination bug would cause infinite looping or missed records.

---

### [high] `kb_regenerate_md` tool has zero tests
**What's missing:** `src/db/markdown.rs` — both `generate_summary` and `regenerate_file` — are completely untested. No test verifies the markdown output format, the visibility filter (`visibility = 'standard'` on all markdown queries), file write, or parent directory creation.

---

### [high] `AlertType::StaleContact` never triggered in any test
**What's missing:** `src/db/alerts.rs::check_stale_contacts` queries `entity.attrs.last_contact`. No test creates an entity with a `last_contact` attribute older than the configured threshold and verifies `AlertType::StaleContact` is returned. The `check_all` integration tests in `alerts_and_status.rs` cover three of the four alert subtypes but skip this one.

---

### [medium] `recall_at` with project/domain filters not tested
**What's missing:** `tests/tool_tests.rs::recall_at_filters_by_created_at` only tests the `as_of` cutoff with no project/domain filters. The project filter path in `recall_at` (which adds a visibility constraint of `visibility != 'sensitive'`) is never exercised.

---

### [medium] BM25 + vector hybrid fusion path never tested end-to-end
**What's missing:** All `search::recall` tests pass `None` for `query_vector`. The RRF fusion code path (steps 2-3 in the pipeline) is never reached. The `rrf_fuse` function itself has no unit test.

**Why it matters:** `rrf_fuse` uses a `HashMap<String, f64>` for score accumulation. A memory appearing only in vector results (not BM25) relies entirely on this path. A bug in the `rrf_score` formula or the HashMap accumulation would never be caught.

---

### [medium] Chunking integration not tested (chunk_and_embed)
**What's missing:** `src/db/chunking.rs` has unit tests for `split_into_chunks` only. No test calls `chunk_and_embed` with a memory that has content ≥ `CHUNK_THRESHOLD` (2000 chars) and verifies:
- Chunk sub-memories are created in the DB
- `extends` edges link each chunk back to the parent
- The chunk memories inherit project/domain/tags from the parent

---

### [medium] `kb_people` tool not tested
**What's missing:** `server.rs` registers a `kb_people` tool (searches entity table with `kind=person`). No test exercises this via the tool layer. Entity find tests exist at the DB layer but not specifically for the `kb_people` path.

---

### [medium] Spreading activation does not test `involves`-via-entity path
**What's missing:** `tests/search_retrieval.rs::recall_includes_graph_neighbors_via_spreading_activation` tests the `extends` edge traversal. The `involves`-via-entity path (memory → involves → entity ← involves ← memory, i.e., "other memories about the same person") is never tested in spreading activation.

---

### [medium] `summary` unicode-only content near 10-char boundary
**What's missing:** `validate_summary` checks `trimmed.len()` (byte length, not char length for unicode). A string like `"日本語テスト"` is 18 bytes but only 6 chars. A 10-byte string of multi-byte characters (e.g., 5× "あ" = 15 bytes) would pass the length check but the intent is 10 characters. The `is_alphanumeric()` check also passes for unicode. No test verifies these boundaries.

---

### [medium] `supersede` on non-existent ID has no test
**What's missing:** `memory::supersede` does not check if the record exists before updating (unlike `memory::archive` which uses `RETURN BEFORE` to detect missing records). `supersede` on a fake ID silently succeeds. No test verifies this discrepancy, which is itself a bug: `supersede` should return `NotFound` for a non-existent ID.

---

### [medium] Export with `exclude_sensitive=true` not verified
**What's missing:** `tests/export_import.rs` calls `export_all(db, dir, false)`. The `exclude_sensitive=true` code path (which adds `WHERE visibility != 'sensitive'`) is never tested. Creating a sensitive memory and then verifying it does not appear in the export with exclusion enabled would catch filter bugs.

---

### [medium] Consolidate with only 1 or 2 IDs not tested
**What's missing:** `tool_tests.rs::consolidate_empty_ids_returns_error` tests the empty-list case, and `consolidate_merges_and_archives_originals` tests 3 IDs. The 1-ID case (degenerate consolidation) is untested; `db::consolidation` and `tools::consolidate` may or may not handle it gracefully.

---

### [low] `until` filter in recall never tested
**What's missing:** `search::recall` accepts an `until` parameter (upper bound on `event_at`). No test provides a non-nil `until` value. The `recall_time_range_filter` test in `tool_tests.rs` only uses `since`.

---

### [low] `recall_at` domain filter not tested
**What's missing:** `recall_at` accepts a `domain` filter but no test passes a non-nil domain. The domain binding in `src/db/search.rs::recall_at` (around line 629) is untested.

---

### [low] `link_nonexistent_memories` not tested
**What's missing:** `tool_tests.rs::link_creates_relation_between_memories` only tests the happy path. No test calls `kb_link` with one or both IDs pointing to non-existent memories. The `graph::link_memories` function calls `RELATE` which may or may not return an error for non-existent records (depends on SurrealDB behavior).

---

### [low] SM-2 ease_factor increase on successful recall not tested
**What's missing:** `src/db/search.rs::compute_sm2` computes `new_ef = (ef + 0.1).min(4.0)` on every access. There's no test that calls `search::recall`, waits for the background SM-2 update, and verifies the `ease_factor` and `review_interval` increased on the accessed memory.

---

### [low] `Importance::new(0)` vs floor enforcement interaction
**What's missing:** `remember::execute` creates the `Importance` from the raw u8 BEFORE calling `enforce_importance`. `Importance::new(0)` returns `Err`, so an input of `importance=0` triggers the `InvalidInput` error path. But there is no test for `importance=0` specifically (only for `importance=6`).

---

## Recommended Tests to Add

Priority ordering: critical first, then high, medium, low.

### 1. [critical] Visibility enforcement in cross-project recall
```rust
// tests/search_retrieval.rs or a new tests/visibility.rs
async fn private_memory_hidden_without_project_filter()
async fn sensitive_memory_hidden_with_project_filter()
async fn private_memory_visible_within_project()
```
Create memories with `Visibility::Private` and `Visibility::Sensitive`, then call `search::recall` both with and without a matching project filter. Assert the sensitive memory never appears, and the private memory only appears when the project filter is set.

### 2. [critical] Dedup thresholds triggered end-to-end
```rust
// tests/memory_crud.rs or tests/dedup.rs
async fn dedup_rejects_duplicate_above_threshold()
async fn dedup_warns_near_duplicate()
```
Store two memories with manually-set identical vectors. Call `dedup::check_duplicate` with a query vector at 0.93 similarity → expect `DedupResult::Duplicate`. Call with 0.85 similarity → expect `DedupResult::Warning`. Also test via `remember::execute` to verify the error propagates and the message string includes "warning: near-duplicate".

### 3. [critical] Supersession of non-active target skips silently
```rust
// tests/tool_tests.rs
async fn remember_supersession_of_archived_target_is_skipped()
async fn remember_supersession_of_superseded_target_is_skipped()
```
Create memory A, archive it. Then call `remember::execute` with `updates_id = A.id`. Verify the new memory is stored (Ok), A's status is still Archived (not double-archived), and no extra supersession edge was created.

### 4. [critical] Import roundtrip
```rust
// tests/export_import.rs
async fn export_import_full_roundtrip()
async fn import_handles_corrupt_line_gracefully()
async fn import_unknown_entry_type_is_skipped()
```
Export 3 memories and 1 entity. Import the file into a fresh DB. Verify all memories are present and entity count matches. Create a file with one valid JSON line, one malformed line, and one with unknown entry_type; verify `errors.len() == 2` and `memories_imported == 1`.

### 5. [high] Signal and Hypothesis TTLs via memory::create
```rust
// tests/memory_crud.rs
async fn working_signal_gets_14d_ttl()
async fn working_hypothesis_gets_7d_ttl()
```
Mirrors the existing `working_priority_gets_7d_ttl` pattern.

### 6. [high] Importance floors via remember::execute for all subtypes
```rust
// tests/tool_tests.rs
async fn remember_life_goal_enforces_importance_5()
async fn remember_goal_enforces_importance_floor_4()
async fn remember_daily_log_enforces_importance_ceiling_2()
async fn remember_action_item_enforces_importance_floor_3()
```
Pass `importance=1` for LifeGoal and verify drilled memory has importance=5. Pass `importance=5` for DailyLog and verify it's clamped to 2.

### 7. [high] Auto-link thresholds with controlled vectors
```rust
// tests/search_retrieval.rs or new tests/auto_link.rs
async fn auto_link_same_project_extends_above_0_90()
async fn auto_link_same_project_relates_between_0_75_and_0_90()
async fn auto_link_cross_project_no_edge_below_0_85()
async fn auto_link_cross_project_relates_above_0_85()
```
Manually set vectors on memories, call `auto_link::auto_link_similar`, then query the graph edges to verify the right edge type was created (or not).

### 8. [high] word_boundary_match edge cases (unit test in src/db/auto_link.rs)
```rust
#[cfg(test)]
mod tests {
    async fn name_at_start_matches()       // "Alice is here" → match "Alice"
    async fn name_at_end_matches()         // "talked to Alice" → match "Alice"
    async fn name_embedded_does_not_match()// "Plaintext" → no match "ain"
    async fn short_name_skipped()          // "Bob" (3 chars) → skipped
    async fn name_adjacent_to_unicode_char() // "Ünterground Alice" → boundary check
}
```

### 9. [high] ConsolidationCandidates tool
```rust
// tests/consolidation.rs
async fn consolidation_candidates_detects_project_subtype_cluster()
async fn consolidation_candidates_detects_graph_cluster()
async fn consolidation_candidates_empty_db_returns_empty()
```
Create 4 active memories in the same project+subtype, call `db::consolidation::find_candidates`. Verify one candidate with 4 ids is returned.

### 10. [high] StaleContact alert
```rust
// tests/alerts_and_status.rs
async fn alerts_detect_stale_contact()
async fn alerts_ignores_recent_contact()
```
Create an entity with `attrs.last_contact` set to 20 days ago (via raw DB query). Set `stale_contact_days=14`. Call `alerts::check_all`. Verify `AlertType::StaleContact` is present.

### 11. [medium] Visibility enforcement in recall_at
```rust
async fn recall_at_excludes_sensitive_without_project()
async fn recall_at_allows_private_with_project_filter()
```

### 12. [medium] BM25 + vector hybrid fusion (rrf_fuse unit test)
```rust
// src/db/search.rs  #[cfg(test)] mod tests
fn rrf_fuse_item_in_both_lists_scores_higher()
fn rrf_fuse_item_only_in_vector_list_included()
fn rrf_fuse_respects_limit()
```

### 13. [medium] Chunking integration
```rust
// tests/memory_crud.rs or new tests/chunking.rs
async fn chunk_and_embed_creates_child_memories_with_extends_edges()
```
Create a memory with `content.len() >= 2000`, call `chunking::chunk_and_embed`, verify child memory IDs exist in DB and each has an `extends` edge to the parent.

### 14. [medium] Export with sensitive exclusion
```rust
// tests/export_import.rs
async fn export_excludes_sensitive_memories_when_flag_set()
```
Create one standard and one sensitive memory. Export with `exclude_sensitive=true`. Verify `memory_count=1` and the exported file does not contain the sensitive summary.

### 15. [medium] supersede on non-existent ID returns error (bug documentation test)
```rust
// tests/memory_crud.rs
async fn supersede_nonexistent_returns_error()  // documents current silent-success bug
```
