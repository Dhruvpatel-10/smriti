# Concurrency & Async Safety Audit

**Date:** 2026-04-10
**Scope:** src/main.rs, src/server.rs, src/tools/remember.rs, src/db/search.rs, src/db/embeddings/ (trait + backends), src/db/auto_link.rs, src/db/graph.rs, src/db/prune.rs
**Modes:** stdio (single session + background tasks), HTTP (multi-session, shared Arc'd SurrealClient)

---

## Summary

The codebase is largely safe for stdio transport and mostly safe for HTTP. No critical safety violations or data corruption risks were found. All tokio::spawn usage is fire-and-forget with proper error logging. The embedding Mutex handles poisoning gracefully. All findings are closed.

**All findings closed as of 2026-04-21 (ADR-009 v0.3.3 hardening).**

---

## Audit Rubric

When reviewing concurrency and async safety:
1. No `Mutex` held across an `.await` point — use `std::sync::Mutex` inside `spawn_blocking` only, or `tokio::sync::Mutex` for async contexts.
2. `tokio::spawn` JoinHandles must either be awaited or have errors logged inside the task before the handle is dropped.
3. Idempotent operations (RELATE, entity upsert) must have a safety net beyond the check-then-act pattern — unique indexes, WHERE guards, or conditional UPDATE.
4. Supersession-class operations (check-then-write) must be atomic — `WHERE status = 'active'` in the UPDATE, not a pre-check + unconditional UPDATE.
5. Fire-and-forget background tasks must not share mutable state with the caller thread.
6. `OnceLock` initialization: if the initializer panics, the OnceLock remains unset and retries on next call — acceptable only if retry is safe.

---

## Findings

### [CLOSED] src/tools/remember.rs — Post-create task tracking: fire-and-forget tokio::spawn without JoinSet
**Closed 2026-04-21 — ADR-009 Fix 3: `KbServer` now carries a `JoinSet<()>` field. All post-create spawns (chunk_and_embed, auto_link_similar, entity match) are added to the set. stdio run loop now uses `tokio::select!` on `tokio::signal::ctrl_c()` and drains the JoinSet with a 5-second bounded timeout before shutdown. Tasks exceeding the drain are logged (IDs only) and abandoned. See src/server.rs and src/main.rs.**

### [CLOSED] src/tools/remember.rs — Stale reads: memory visible before post-create tasks complete
**Closed 2026-04-20 — Accepted by design per CLAUDE.md ("Drain session is OPTIONAL for correctness"). Memory is immediately queryable via BM25 after create; vector and graph edges arrive asynchronously. A code comment was added at the post-create phase citing the ADR.**

### [CLOSED] src/db/graph.rs:32-35 — TOCTOU race in duplicate edge check
**Closed 2026-04-20 — All edge tables have `UNIQUE` indexes on `(in, out)` (migration 002). If both RELATE statements execute concurrently, one succeeds and the other fails silently via `unwrap_or_default()` — the correct behavior for idempotent edge creation. Net result: exactly one edge.**

### [CLOSED] src/tools/remember.rs — Supersession race: two concurrent updates_id targeting the same memory
**Closed 2026-04-20 — Supersede-first ordering implemented. `memory::supersede` (conditional `UPDATE ... WHERE status = 'active' RETURN BEFORE`) now executes before `memory::create`. The second concurrent call sees the old memory already superseded and receives `InvalidInput` before writing any new record. Rollback guard: `memory::unsupersede` restores old memory to active if create fails after a successful supersede. (ADR-011 2026-04-21 updated guard to `WHERE invalidated_at IS NONE`; rollback now also clears bitemporal fields.)**

### [CLOSED via ADR-009] src/tools/consolidate.rs:42-67 — kb_consolidate non-atomic transaction
**Closed 2026-04-21 — ADR-009 Fix 2: Full consolidation sequence (create consolidated memory, archive sources, relate edges) wrapped in SurrealQL `BEGIN TRANSACTION...COMMIT` block. Additionally: reject `ids.len() > 50` with `InvalidInput`. Test coverage: consolidate-txn-rollback verifies atomicity; consolidate-ids-cap verifies cap. See tests/consolidation.rs.**

### [CLOSED via ADR-009] src/main.rs:133-145 — stdio shutdown has no ctrl_c handler
**Closed 2026-04-21 — ADR-009 Fix 3: stdio run loop now uses `tokio::select!` on `tokio::signal::ctrl_c()` + service future. See src/main.rs lines 133-145.**

### [CLOSED via ADR-009] src/db/prune.rs, src/main.rs — periodic loops untracked, not cancelled on shutdown
**Closed 2026-04-21 — ADR-009 Fix 3: Periodic prune and consolidation loops now use `CancellationToken` from `tokio-util`. Main loop cancels and awaits on shutdown signal. See src/main.rs and src/db/prune.rs.**

### [INFO] Auto-consolidation loop — isolated SidecarClient instance (ADR-026)
The auto-consolidation periodic loop (ADR-026) uses a **second `SidecarClient` instance**
separate from the one used by `extract_relations`/`extract_anchors` in the `kb_remember`
post-create path. The two instances have independent supervisor loops and independent
circuit breakers. A circuit-breaker trip caused by consolidation sidecar failures does
not disable extraction on new `kb_remember` calls, and vice versa. Both instances share
the same `CancellationToken` for shutdown coordination.

### [SAFE] src/db/embeddings/local.rs — OnceLock + Mutex pattern for model loading
**Verdict: Safe.** `OnceLock::get_or_init` ensures the model loads exactly once. `Mutex<TextEmbedding>` serializes access. Poison handling converts `PoisonError` to `KbError::Embedding` rather than panicking. `embed()` uses `tokio::task::spawn_blocking` — Mutex never held across an await point. (`src/db/embeddings.rs` was refactored into the `embeddings/` module tree; finding applies to `local.rs`.)

### [SAFE] src/db/prune.rs — Pruning task archiving memories during concurrent reads
**Verdict: Safe.** SurrealDB (SurrealKV) provides snapshot isolation at the query level. A recall query that starts before a prune UPDATE completes sees the pre-update state. No torn reads possible.

### [ACCEPTED BY DESIGN] src/db/search.rs:564-578 — SM-2 update race: concurrent access_count and SM-2 field updates
**Accepted 2026-04-20 — The two concurrent UPDATE tasks write disjoint fields (access_count/updated_at vs. ease_factor/review_interval/next_review_at). No write-write conflict. SM-2 lost-update under concurrent recalls of the same memory is inconsequential for a personal KB — values converge over time.**

### [SAFE] SurrealDB concurrency model: Surreal<Db> (SurrealKV) thread safety
**Verdict: Safe.** `Surreal<Db>` is `Send + Sync`, Arc-shared internally, designed for concurrent use from multiple tokio tasks. SurrealKV uses MVCC for concurrent reads and serialized writes.

### [CLOSED] src/tools/remember.rs:152-154 — Entity linking is synchronous in the write path
**Closed 2026-04-20 — Accepted by design per CLAUDE.md. Synchronous entity linking is intentional; the latency impact for memories with many people is acceptable for the single-user local-first use case.**

### [CLOSED] src/tools/remember.rs:202-223 — Spawned chunking task re-fetches memory from DB
**Closed 2026-04-20 — `ChunkParent` struct now passes summary, project, domain, tags, source_agent, and visibility directly from the create path into the spawn closure; no DB re-fetch, no race window.**
