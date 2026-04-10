# Memory Usage & Performance Audit

## Summary

The server is well-architected for a personal-scale knowledge base. There are no catastrophic memory leaks or unbounded resource usage. However, there are several medium-severity allocation inefficiencies and one high-severity issue in the consolidation pipeline that will degrade performance as the dataset grows. The ONNX model is loaded and shared correctly. Background task spawning is bounded by the rate of incoming requests (no unbounded fan-out). The `Mutex<TextEmbedding>` architecture serializes all inference, which is fine for a personal single-user server but would be a bottleneck under concurrent HTTP load.

---

## Findings

### [HIGH] src/db/consolidation.rs:82-125 — O(n²) in-memory summary lookup

The graph-cluster heuristic fetches up to 200 raw memory rows from the DB (`LIMIT 200`), then for each of those rows that qualifies, it iterates the full `rows` Vec again to find summaries of related memories:

```rust
if let Some(s) = rows
    .iter()
    .find(|r| r.get("mid") ... == Some(rid.as_str()))
```

At 200 rows this is up to 200 × 200 = 40,000 iterations of deserialized `serde_json::Value` comparisons. Each row is a full heap-allocated JSON object. The result is O(n²) time and retains all 200 full JSON rows in memory simultaneously. A simple `HashMap<&str, &str>` keyed on `mid` would reduce this to O(n) with a single pass.

---

### [MEDIUM] src/db/search.rs:31-52 — Double HashMap allocation in `rrf_fuse` with redundant string cloning

`rrf_fuse` allocates two `HashMap<String, _>` entries per result. The ID is extracted from the `MemorySummary`, `.to_string()`-ed, then `.clone()`-ed to serve as both the scores key and the by_id key:

```rust
let id = mem.id.as_str().to_string();
*scores.entry(id.clone()).or_default() += rrf_score(rank);
by_id.entry(id).or_insert_with(|| mem.clone());
```

`mem.clone()` is called for every entry inserted into `by_id`. `MemorySummary` likely contains at least `String` fields for id, summary, project, domain tags — potentially several hundred bytes per clone. For 25+25=50 inputs this is modest, but the pattern is wasteful. Using `entry(id.clone()).or_insert_with(|| mem.clone())` only for the case where the key is absent would be fine, but the `mem.clone()` on every `or_insert_with` is already lazily evaluated — the real issue is the redundant `id.clone()` allocation on every iteration. Using a single `HashMap<String, (f64, MemorySummary)>` would halve the number of heap allocations.

---

### [MEDIUM] src/db/search.rs:541-556 — Linear scan in `primary_ids.contains()` inside spreading activation dedup

After spreading activation returns neighbors, each neighbor ID is checked against `primary_ids` (a `Vec<String>`) with `.contains()`, which is O(n) per check:

```rust
if !primary_ids.contains(&n.id.as_str().to_string())
```

`n.id.as_str().to_string()` also allocates a new `String` per check. `primary_ids` is already constructed a few lines above — converting it to a `HashSet<&str>` before this loop would eliminate both the O(n) scan and the per-iteration allocation.

---

### [MEDIUM] src/db/search.rs:286-312 — Dynamic SQL string construction per request

The BM25 and vector SQL queries are assembled via `format!()` macros on every call to `recall()`. This allocates fresh `String` buffers for each query, including a `Vec<String>` for `conditions`, multiple `to_string()` calls for filter fragments, and a final `.join()`. For a personal server this is not a real bottleneck, but the pattern is unnecessary: the filter combination space is small (6 binary flags). These could be pre-computed as `&'static str` constants, or at minimum the conditions `Vec` pre-allocated with `Vec::with_capacity(7)` since the maximum number of conditions is known.

---

### [MEDIUM] src/db/auto_link.rs:74-88 — One DB round-trip per similar memory for project check

In `auto_link_similar`, for each of up to 15 candidate similar memories, `check_same_project` fires a separate DB query:

```rust
let same_project = if let Some(proj) = memory_project {
    check_same_project(db, &similar_id, proj).await
```

This is up to 15 sequential DB round-trips. The project field is already returned in `find_similar`'s query (`SELECT record::id(id) AS mem_id, project, ...`), but it is discarded. Returning project as part of `find_similar`'s result tuple would eliminate all 15 queries.

---

### [MEDIUM] src/db/embeddings.rs:157-163 — Unnecessary `.to_vec()` clone when storing vector

`store_vector` takes `vector: &[f32]` but immediately clones it via `.to_vec()` when binding:

```rust
.bind(("vector", vector.to_vec()))
```

This allocates a new `Vec<f32>` (256 × 4 = 1024 bytes for the default 256-dim model) on every store call. Whether surrealdb's `.bind()` requires ownership is worth checking — if it accepts `impl Serialize`, a wrapper that serializes the slice directly would avoid the copy. The same pattern appears in `dedup::check_duplicate` and `dedup::find_similar`.

---

### [MEDIUM] src/db/chunking.rs:93-119 — Full parent `Memory` re-fetched after just being created

In `remember.rs`, after creating a memory, a background task fetches the full parent record again (`memory::get(&chunk_db, &chunk_parent_id)`) before chunking. The `Memory` struct is already available in `execute()` at the point of creation — it could be cloned and moved into the spawn closure, eliminating the DB round-trip entirely.

---

### [LOW] src/db/embeddings.rs:92-117 — `Mutex<TextEmbedding>` serializes all inference

The ONNX model is wrapped in a `Mutex`. In stdio mode (one session) this is fine. In HTTP daemon mode, concurrent recall calls that all trigger embedding will queue on this mutex, serializing inference end-to-end. For a personal server with low concurrency this is acceptable. If concurrent HTTP sessions become common, a `tokio::sync::Semaphore`-bounded pool of model instances (or a dedicated embedding worker channel) would improve throughput without increasing memory proportionally.

---

### [LOW] src/db/search.rs:60-82 — One `tokio::spawn` per SM-2 update, unbounded under bursty load

`spawn_sm2_access_update` fires an unconditional `tokio::spawn` for every recall that returns BM25 results. Similarly, the access-count update at line 564-575 spawns unconditionally. Under a burst of recall calls (e.g., agent doing many `kb_recall` in sequence), this creates one task per recall on the Tokio runtime. Each task holds a `SurrealClient` clone (Arc bump) and a `Vec<Sm2Update>`. The tasks are short-lived and the client is shared via `Arc`, so actual memory impact is small — but the spawn count is proportional to recall rate with no back-pressure. Batching SM-2 updates (e.g., via a channel to a single worker) would cap memory and reduce DB write amplification.

---

### [LOW] src/db/prune.rs:72-77, 141-143, 168 — Full `RETURN AFTER` on bulk UPDATE fetches entire result set

Both `expire_ttl` and `sm2_decay` use `RETURN AFTER`, which returns the full updated records. The code only uses `rows.len()` as a count. Using `RETURN NONE` (or `RETURN id` for a minimal payload) would avoid deserializing and heap-allocating every updated record's JSON representation just to count them. On a run that expires 500 memories this unnecessarily allocates 500 full JSON objects.

---

### [LOW] src/db/auto_link.rs:101-103 — Full entity table scan on every memory write

`match_known_entities` executes `SELECT name FROM entity` with no filter, fetching every known entity name on each memory creation. For a personal KB this may be O(hundreds) of rows, but the result is neither cached nor paginated. As the entity table grows, this becomes a progressively more expensive query on what is already a post-write hot path. A simple in-process cache (e.g., `Arc<RwLock<Vec<String>>>` invalidated on entity upsert) would reduce this to a memory read for the common case.

---

### [LOW] src/main.rs:83-105 — Consolidation background task materializes full candidate set every N hours, result is only logged

`find_candidates` in the periodic task materializes up to `limit=10` candidates (each containing Vec<String> for ids and summaries), logs a count, and discards the data. The query for Heuristic 2 fetches up to 200 full memory rows to do in-memory graph analysis. This runs every `consolidation_interval_hours` even when the KB is quiescent. The result is immediately discarded — the candidates are only useful when a user calls `kb_consolidation_candidates`. Running this query only on-demand (removing the background task, or caching results with a staleness TTL) would avoid the periodic 200-row materialization.

---

### [LOW] src/db/search.rs:501 — Redundant `HashSet` construction for entity-anchored filter

In the entity-anchored path, `text_ids` is built as a `HashSet<String>` from `text_ranked`, but `text_ranked` is itself derived from `memories` which was already filtered. The set construction iterates and allocates when `text_ranked.len()` is at most `limit_usize` (≤25). Minor, but the pattern of building a new set from a just-filtered slice is a signal that the logic could be restructured into a single pass.

---

### [INFO] Cargo.toml — `rig-core` pulled in as a dependency

`rig-core` is listed as a dependency (`version = "0.34"`). Unless it is actively used for HTTP LLM calls, it contributes significant compile-time cost and binary size (it transitively pulls in `reqwest`, `rustls`, TLS stacks, and JSON schema machinery). Verify it is needed in the hot path; if it is only used in a few tool handlers, consider feature-flagging it or replacing the specific functionality used with a direct `reqwest` call.

---

## Recommendations

**Priority 1 — Fix before the KB grows large:**
1. Replace the O(n²) `rows.iter().find()` loop in `consolidation.rs` with a `HashMap<&str, &str>` built in one pass.
2. Return `project` from `find_similar` and eliminate the 15 per-similarity-candidate DB queries in `auto_link_similar`.
3. Change `RETURN AFTER` to `RETURN NONE` in `expire_ttl`, `sm2_decay`, and `archive_below_threshold` — count via `rows.len()` on `RETURN id` if needed, or just issue a separate `SELECT count()`.

**Priority 2 — Allocation efficiency:**
4. Consolidate the two `HashMap`s in `rrf_fuse` into one `HashMap<String, (f64, MemorySummary)>`.
5. Replace the `Vec<String>::contains` linear scan after spreading activation with a `HashSet<&str>`.
6. Pass the already-constructed `Memory` into the chunking background task rather than re-fetching it from the DB.
7. Pre-allocate `conditions` Vec with `Vec::with_capacity(7)` in `recall()`.

**Priority 3 — Background task hygiene:**
8. Cache the entity name list (invalidated on `kb_entity_upsert`) to avoid a full-table scan on every `kb_remember`.
9. Remove the periodic consolidation background task or cache its result; run analysis only on-demand.
10. Consider batching SM-2 and access-count updates through a bounded channel rather than spawning one task per recall.

**Priority 4 — Architecture (lower urgency):**
11. In HTTP daemon mode, if concurrent embedding becomes a bottleneck, replace `Mutex<TextEmbedding>` with a small bounded pool or a dedicated worker task.
12. Audit `rig-core` usage — if it is only used for a handful of API calls, remove the dependency and use `reqwest` directly to reduce binary size and compile time.
