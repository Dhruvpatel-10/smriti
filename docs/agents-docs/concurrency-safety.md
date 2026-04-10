# Concurrency & Async Safety Audit

**Date:** 2026-04-10
**Scope:** src/main.rs, src/server.rs, src/tools/remember.rs, src/db/search.rs, src/db/embeddings.rs, src/db/auto_link.rs, src/db/graph.rs, src/db/prune.rs
**Modes:** stdio (single session + background tasks), HTTP (multi-session, shared Arc'd SurrealClient)

---

## Summary

The codebase is **largely safe for the stdio transport** and **mostly safe for HTTP**, with several low-to-medium severity issues. The most impactful finding is that stale reads are architecturally inherent (memory visible before vector/edges exist) but this is a deliberate design choice documented by the "embed-first, create-last" pipeline. The duplicate edge TOCTOU race in graph.rs is mitigated by unique indexes. The supersession race has a real but narrow window for double-supersession. The SM-2 update race is a minor data-quality issue, not a correctness hazard.

No critical safety violations or data corruption risks were found. All tokio::spawn usage is fire-and-forget with proper error logging, and the embedding Mutex handles poisoning gracefully.

---

## Findings

### [LOW] src/tools/remember.rs:182-196 -- Stale reads: memory visible before post-create tasks complete

**Issue:** After `memory::create()` returns (line 147), the memory is immediately queryable via `kb_recall`. However, the vector is stored at line 185, auto-linking spawns at line 190, and chunking spawns at line 202. A concurrent `kb_recall` issued between lines 147 and 185 will find the memory via BM25 but it will have no vector (so vector search misses it) and no graph edges (so spreading activation misses it).

**Why this is LOW, not MEDIUM:** The write path is designed as "embed-first, create-last" -- the vector is *computed* before creation (dedup check at line 96 uses the pre-computed vector). The *storage* of the vector is the only post-create step, and it executes synchronously (not spawned) at line 185. Only auto-linking and chunking are truly fire-and-forget spawned tasks. The window for a missing vector is therefore very small (just the await on `store_vector_pub`). Auto-linking edges being absent briefly is cosmetically imperfect but does not cause incorrect results -- it just means spreading activation returns fewer neighbors temporarily.

**Failure scenario:** User does `kb_remember` then immediately `kb_recall` with vector search. The BM25 path finds the memory; the vector path may miss it if `store_vector_pub` hasn't completed. Result: the memory still appears (via BM25/RRF fusion), just with a slightly lower fused rank.

**Recommendation:** Acceptable as-is. If strict consistency is ever needed, move `store_vector_pub` before `memory::create` is not possible (need the ID), but you could make the remember handler await all post-create work before returning. This would increase latency for no practical benefit in a personal KB.

---

### [LOW] src/db/graph.rs:32-35 -- TOCTOU race in duplicate edge check

**Issue:** The `link_memories` function uses a two-step pattern:
```sql
LET $existing = (SELECT * FROM {table} WHERE in = $from AND out = $to);
IF $existing = [] { RELATE $from -> {table} -> $to };
```
Under concurrent writes, two tasks could both execute the SELECT and find no existing edge, then both issue the RELATE.

**Why this is LOW, not MEDIUM:** All edge tables have `UNIQUE` indexes on `(in, out)` (migration 002, lines 116-121 in migrations.rs). SurrealDB's unique index constraint is the hard safety net. If both RELATE statements execute, one succeeds and the other fails with a unique constraint violation. The current code does consume the response (`response.take(1usize)`) but discards errors from it (`unwrap_or_default()`). This means the second RELATE silently fails, which is the correct behavior for idempotent edge creation.

**Failure scenario:** Two concurrent `kb_remember` calls auto-link to the same target memory. Both pass the SELECT check, both attempt RELATE. One succeeds, one gets a unique constraint error that is silently swallowed. Net result: exactly one edge exists. Correct.

**Recommendation:** Safe due to the unique index. Optionally, log a trace-level message when the RELATE response contains an error, to aid debugging. The SELECT pre-check is still valuable as it avoids the error path in the common case.

---

### [MEDIUM] src/tools/remember.rs:163-179 -- Supersession race: two concurrent updates_id targeting the same memory

**Issue:** When two concurrent `kb_remember` calls both specify the same `updates_id`, both execute this sequence:
1. `memory::get(db, &old_id)` -- both see `status == Active`
2. `memory::supersede(db, &old_id)` -- both UPDATE to `status = 'superseded'`
3. `graph::link_memories(...)` -- both create an `updates` edge

The result: **two new memories both claim to supersede the same old memory**. The old memory is correctly superseded (the second UPDATE is a no-op since it's already superseded), but the graph now has two `updates` edges pointing from different new memories to the same old memory. There is no unique constraint preventing this because the `updates` index is on `(in, out)` -- the two edges have different `in` values (different new memory IDs).

**Failure scenario:** Client sends two `kb_remember` calls rapidly with `updates_id: "abc123"`. Both succeed. The knowledge base now contains two active memories that both supersede memory `abc123`. A later `kb_recall` returns both, which is confusing -- the user sees two versions of what should be one updated memory.

**Why this is MEDIUM, not HIGH:** In practice, MCP tool calls from a single Claude session are sequential (Claude waits for the response before sending the next tool call). This race requires two independent sessions in HTTP mode both updating the same memory simultaneously, which is unlikely for a personal KB. Additionally, no data corruption occurs -- just a logical inconsistency.

**Recommendation:** Add a conditional check to the supersede UPDATE:
```sql
UPDATE type::record('memory', $id)
SET status = 'superseded', updated_at = time::now()
WHERE status = 'active'
RETURN AFTER
```
Then check if the returned row is empty (meaning someone else already superseded it) and skip the `link_memories` call if so. This makes the supersession atomic with respect to the status check.

---

### [SAFE] src/db/embeddings.rs:22-26 -- OnceLock + Mutex pattern for model loading

**Mechanism:** `LocalEmbedder` uses `Arc<OnceLock<Result<Mutex<TextEmbedding>, String>>>`. The `OnceLock::get_or_init` (line 57) ensures the model loads exactly once, even under concurrent calls. The `Mutex<TextEmbedding>` serializes access to the `embed()` method (which requires `&mut self` per fastembed's API).

**Panic safety:** Line 99 handles `Mutex::lock()` returning a `PoisonError` by converting it to `KbError::Embedding`. If a thread panics while holding the Mutex, subsequent callers get a clear error message rather than a panic. The OnceLock stores the error case too (line 69), so if the model fails to load, all future callers get the same error without retrying.

**Async safety:** The `embed()` method (line 86) correctly uses `tokio::task::spawn_blocking` to avoid blocking the async runtime during ONNX inference. The Mutex is a `std::sync::Mutex` (not `tokio::sync::Mutex`), which is correct here because the lock is held only inside `spawn_blocking` -- never across an await point.

**One concern:** If the model load itself panics (inside `TextEmbedding::try_new`), `OnceLock::get_or_init` will propagate the panic to the first caller, and subsequent callers will find the OnceLock still uninitialized (OnceLock does not store values from panicked initializers). This means they'd retry the load, which could succeed or panic again. This is acceptable -- the panic would only happen during startup if the ONNX runtime has a fatal error.

**Verdict:** Safe for concurrent use. No issues.

---

### [SAFE] src/db/prune.rs -- Pruning task archiving memories during concurrent reads

**Mechanism:** The pruning task (main.rs line 71) calls `prune::run_all` which executes UPDATE statements to set `status = 'archived'` on expired/decayed memories. A concurrent `kb_recall` filters on `WHERE status = 'active'`.

**Why this is safe:** SurrealDB (embedded SurrealKV mode) provides snapshot isolation at the query level. A recall query that starts before a prune UPDATE completes will see the pre-update state -- the memory appears active for that query. A recall query that starts after the prune completes will see the memory as archived and filter it out. There is no intermediate "torn" state.

**Edge case:** A memory could be returned by recall at the exact moment pruning archives it. The user sees a memory that is "about to expire." This is not a bug -- it's the expected behavior of eventually-consistent TTL systems. The next recall will not return it.

**Verdict:** Safe. No action needed.

---

### [LOW] src/db/search.rs:564-578 -- SM-2 update race: concurrent access_count and SM-2 field updates

**Issue:** After a recall, two background tasks are spawned:
1. Line 566: `access_count += 1, updated_at = time::now()` for all returned memories
2. Line 578: `spawn_sm2_access_update` which updates `ease_factor`, `review_interval`, `next_review_at` for BM25-matched memories

These two UPDATE statements run concurrently on overlapping rows. However, they update **disjoint fields**: task 1 writes `access_count` and `updated_at`; task 2 writes `ease_factor`, `review_interval`, and `next_review_at`.

**Potential issue with `updated_at`:** Task 1 sets `updated_at = time::now()`. Task 2 does NOT set `updated_at`. So there is no write-write conflict on any field.

**Potential issue with read-before-write:** The SM-2 task in `compute_sm2` (line 91) reads `ease_factor` and `review_interval` from the **original query result rows** (captured before the background tasks run), not from the database. So the SM-2 computation is based on a consistent snapshot. The access_count task uses `+= 1` which is an atomic increment in SurrealQL.

**Cross-recall race:** If two concurrent recalls both return the same memory, both will spawn `access_count += 1` and both will spawn SM-2 updates. The access_count will be incremented twice (correct). The SM-2 updates will both read the same base values and compute the same new values, then both write them. The second write overwrites the first with identical values. This is idempotent and correct.

**Subtle issue:** If the two recalls happen with slightly different timing, they might read different `ease_factor` values (one pre-update, one post-update from a third recall). This could cause a "lost update" where one SM-2 computation overwrites another. For a personal KB, this is negligible -- SM-2 values converge over time regardless.

**Verdict:** Low risk. The disjoint field updates are safe. The SM-2 lost-update scenario is theoretically possible but inconsequential for the use case.

---

### [SAFE] SurrealDB concurrency model: Surreal<Db> (SurrealKV) thread safety

**Mechanism:** `SurrealClient` is `Surreal<Db>` where `Db` is the embedded SurrealKV engine. The `Surreal` type is `Clone` and internally uses `Arc`-based shared state. The SurrealDB Rust SDK documentation and source confirm that `Surreal<Db>` is `Send + Sync` and safe for concurrent use from multiple tokio tasks.

**Evidence in codebase:** The code clones `db` freely (main.rs lines 69, 86; search.rs line 565; auto_link.rs line 161; remember.rs lines 201-202) and passes clones to `tokio::spawn`. This is the intended usage pattern.

**SurrealKV isolation:** SurrealKV (the embedded engine) uses MVCC for concurrent reads and serialized writes. Multiple concurrent reads are lock-free. Concurrent writes are serialized at the storage layer. This means:
- Two concurrent `kb_remember` calls: their CREATE statements are serialized by the engine. No torn writes.
- A `kb_recall` concurrent with a `kb_remember`: the recall sees a consistent snapshot (either the memory exists or it doesn't).

**Verdict:** Safe. `Surreal<Db>` is designed for this exact usage pattern.

---

### [LOW] src/tools/remember.rs:152-154 -- Entity linking is synchronous in the write path

**Issue:** The `for person in &people` loop (line 152) and `link_assigned` (line 158) execute sequentially and are awaited before the function returns. If a memory has many people (e.g., 10), this adds latency to the remember response. This is not a concurrency bug but an async performance concern.

**Recommendation:** Consider spawning entity linking as a background task (similar to auto-linking). The entity upsert uses `INSERT ... ON DUPLICATE KEY UPDATE` which is idempotent, so fire-and-forget is safe.

---

### [INFO] src/tools/remember.rs:202-223 -- Spawned chunking task re-fetches memory from DB

**Observation:** The chunking task (line 205) calls `memory::get(&chunk_db, &chunk_parent_id)` to re-fetch the memory that was just created. This is necessary because the `content` field was moved into `memory::create()` and is no longer available. There is a theoretical race where the memory could be archived (by a concurrent prune) between creation and the re-fetch. In practice, this is impossible because the memory was just created and no TTL or decay would apply within milliseconds.

**Verdict:** Safe. No action needed.

---

## Recommendations

### Priority 1 (should fix)

1. **Atomic supersession check** (MEDIUM): Change `memory::supersede` to conditionally update only if `status = 'active'`, and check the return value in remember.rs before creating the `updates` graph edge. This closes the only real race condition that can produce user-visible inconsistency.

### Priority 2 (nice to have)

2. **Log suppressed RELATE errors** (LOW): In graph.rs, when `response.take(1)` contains an error (unique constraint violation from a concurrent duplicate RELATE), log it at `trace` level. Currently it's silently swallowed via `unwrap_or_default()`.

3. **Consider spawning entity linking** (LOW): Move the `for person in &people` loop in remember.rs to a spawned task to reduce remember latency for memories with many people. The `INSERT ON DUPLICATE KEY UPDATE` pattern is idempotent and safe for fire-and-forget.

### No action needed

4. **Stale reads** (LOW): Architecturally inherent and acceptable. BM25 provides immediate discoverability; vector search catches up within milliseconds.

5. **SM-2 update race** (LOW): Theoretically possible lost updates on SM-2 fields under concurrent recalls of the same memory. Inconsequential for a personal KB -- values self-correct over time.

6. **OnceLock + Mutex** (SAFE): Correctly implemented with poison handling and spawn_blocking.

7. **SurrealDB concurrency** (SAFE): Surreal<Db> is Send + Sync, MVCC-isolated, designed for multi-task concurrent access.

8. **Prune during read** (SAFE): Snapshot isolation prevents torn reads. Worst case is returning a memory that was archived microseconds ago.
