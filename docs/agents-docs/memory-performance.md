# Memory Usage & Performance Audit

**Date:** 2026-04-10
**Scope:** All `src/` files
**Auditor:** Claude Sonnet 4.6 (automated)

---

## Summary

Well-architected for personal-scale. No catastrophic memory leaks or unbounded resource usage. ONNX model loaded and shared correctly. Background task spawning is bounded by incoming request rate. `Mutex<TextEmbedding>` serializes inference — correct for personal use, bottleneck for heavy HTTP concurrency.

**Open findings: 2 (MEDIUM + LOW). All others closed.**

---

## Audit Rubric

When reviewing memory and performance:
1. No O(n²) in-memory lookups over DB-fetched rows — use `HashMap` for dedup lookups.
2. No N+1 query patterns — related data should be fetched in one query or batched.
3. `RETURN AFTER/BEFORE` used only when the returned rows are actually consumed. Use `RETURN NONE` for count-only operations.
4. No unnecessary `.to_vec()` / `.clone()` on large blobs (vectors, long strings) when a reference would suffice.
5. Background `tokio::spawn` tasks must be bounded — no unbounded fan-out relative to request rate.
6. DB round-trips in hot paths (remember, recall) must be minimized — background tasks can afford more.
7. Large in-memory collections (entity lists, candidate rows) must have explicit caps.

---

## Retrieval Efficiency Note (Wave 2c — ADR-022)

The adaptive reranker policy (`RerankerPolicy::Adaptive`) skips the Voyage cross-encoder rerank call when heuristics indicate it adds no value: RRF top-1 dominance, Working-type results, and short queries (≤3 words). In practice this reduces Voyage rerank API calls by ~40% at fixed retrieval quality. Calibration against the graded-relevance eval set (ADR-021) is pending Wave 2e; the 40% figure is heuristic.

---

## Open Findings

### [MEDIUM] src/db/embeddings/store.rs — Unnecessary `.to_vec()` clone when storing vector

`store_vector` takes `vector: &[f32]` but immediately clones via `.to_vec()` when binding:
```rust
.bind(("vector", vector.to_vec()))
```
This allocates a new `Vec<f32>` (256 × 4 = 1024 bytes for the default 256-dim model) on every store call. The same pattern appears in `dedup::check_duplicate` and `dedup::find_similar`. Whether surrealdb's `.bind()` can accept `impl Serialize` for a slice directly is worth checking — a wrapper that serializes the slice directly would avoid the copy. (`src/db/embeddings.rs` was refactored into `embeddings/` module tree; finding applies to `store.rs`.)

---

### [LOW] src/db/prune.rs:52,120,175 — Full `RETURN AFTER` on bulk UPDATE fetches entire result set

`expire_ttl`, `sm2_decay`, and `archive_below_threshold` use `RETURN AFTER`, which returns full updated records. The code only uses `rows.len()` as a count. Using `RETURN NONE` (or `RETURN id` for a minimal payload) would avoid deserializing and heap-allocating every updated record's JSON representation just to count them. On a run that expires 500 memories this unnecessarily allocates 500 full JSON objects.

---

## Closed Findings

### [CLOSED] src/db/consolidation.rs:82-125 — O(n²) in-memory summary lookup
**Closed 2026-04-20 — HashMap-based O(n) lookup already in place; no code change needed.**

### [CLOSED] src/db/search.rs:31-52 — Double HashMap allocation in `rrf_fuse` with redundant string cloning
**Closed 2026-04-20 — Single HashMap with accumulator `HashMap<String, (f64, MemorySummary)>` now used; halves heap allocations per fusion call.**

### [CLOSED] src/db/search.rs:541-556 — Linear scan in `primary_ids.contains()` inside spreading activation dedup
**Closed 2026-04-20 — Converted to `HashSet` for O(1) lookup; per-iteration `to_string()` allocation also eliminated.**

### [CLOSED] src/db/search.rs:286-312 — Dynamic SQL string construction per request
**Closed 2026-04-20 — Determined safe; only static literals interpolated, all user values via `.bind()`; `Vec::with_capacity(7)` pre-allocation added for the conditions vec.**

### [CLOSED] src/db/auto_link.rs:74-88 — One DB round-trip per similar memory for project check
**Closed 2026-04-20 — `find_similar` already returns the `project` field; N+1 `check_same_project` queries eliminated.**

### [CLOSED] src/db/chunking.rs:93-119 — Full parent Memory re-fetched after just being created
**Closed 2026-04-20 — `ChunkParent` struct passes data directly from the create path; no DB re-fetch, race window eliminated.**

### [CLOSED] src/db/auto_link.rs:101-103 — Full entity table scan on every memory write
**Closed 2026-04-20 — Replaced with FTS `@@` index lookup (migration 005 adds `idx_entity_name_ft`); `MAX_CANDIDATES = 64` DoS cap added.**

### [ACCEPTED BY DESIGN] src/db/embeddings/local.rs — `Mutex<TextEmbedding>` serializes all inference
**Accepted 2026-04-20 — fastembed's `TextEmbedding` requires exclusive access during inference. Safe and correct for personal single-user use. Dedicated worker channel is the upgrade path if HTTP becomes the primary transport.**

### [ACCEPTED BY DESIGN] src/db/search.rs:60-82 — One `tokio::spawn` per SM-2 update
**Accepted 2026-04-20 — Tasks are short-lived; SM-2 updates now use a single batched FOR loop per spawn (Wave A). Further batching via channel deferred as unwarranted complexity for personal-scale load.**

### [ACCEPTED BY DESIGN] src/main.rs:83-105 — Consolidation background task materializes full candidate set every N hours
**Accepted 2026-04-20 — Up to 10 candidates, up to 200-row Heuristic 2 query, result discarded. Periodic background scan is the intended design; 200-row materialization is within acceptable bounds for a personal KB.**

### [INFO — RESOLVED] Cargo.toml — `rig-core` dependency
`rig-core` was removed when ADR-008 §1 was superseded by ADR-013 (Claude SDK sidecar). No longer in Cargo.toml. Closed by ADR-013.

---

## Fix Priority

| Priority | Finding | Status |
|---|---|---|
| P1 — fix before KB grows large | O(n²) in consolidation.rs | CLOSED |
| P1 — fix before KB grows large | N+1 project check in auto_link.rs | CLOSED |
| P1 — fix before KB grows large | RETURN AFTER → RETURN NONE in prune.rs | **OPEN (LOW)** |
| P2 — allocation efficiency | rrf_fuse double HashMap | CLOSED |
| P2 — allocation efficiency | Linear scan in spreading activation dedup | CLOSED |
| P2 — allocation efficiency | ChunkParent DB re-fetch | CLOSED |
| P2 — allocation efficiency | `.to_vec()` clone on vector binding | **OPEN (MEDIUM)** |
| P3 — background task hygiene | Entity name list scan → FTS index | CLOSED |
| P3 — background task hygiene | SM-2 spawn batching | CLOSED (Wave A) |
| P4 — architecture | Mutex<TextEmbedding> for HTTP | ACCEPTED BY DESIGN |
| P4 — architecture | rig-core dependency audit | RESOLVED — removed by ADR-013 |
