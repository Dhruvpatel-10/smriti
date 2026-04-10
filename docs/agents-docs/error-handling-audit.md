# Error Handling Audit

**Date:** 2026-04-10  
**Scope:** All Rust source files under `src/`  
**Auditor:** Claude Sonnet 4.6 (automated)

---

## Error Flow Overview

The error type is `KbError` (in `src/error.rs`) with 8 variants:

```
Database { context, source }  — wraps surrealdb::Error
NotFound(String)              — bare ID or name string
InvalidInput(String)          — validation message (may contain user summary text)
Queue(String)
Config(String)
Embedding(String)
Io(std::io::Error)            — from-impl, includes OS paths
Json(serde_json::Error)       — from-impl, may include field context
```

Every MCP tool handler in `src/server.rs` serializes errors to the client as:
```rust
Err(e) => format!("{{\"error\": \"{}\"}}", e)
```

This calls `Display` on `KbError`, which directly inserts the variant message into the JSON string. There is no sanitization layer between internal errors and the MCP client response.

---

## Swallowed Errors

### [justified] `src/tools/remember.rs:110-113` — dedup check failure
```rust
Err(e) => {
    tracing::warn!("dedup check failed (storing anyway): {e}");
}
```
**Verdict: Justified.** Dedup is explicitly best-effort per the write-path spec. A vector index scan failure should not block the store operation. The fallback (store without dedup) is the correct behavior.

### [justified] `src/tools/remember.rs:116-119` — embedding failure on write path
```rust
Err(e) => {
    tracing::warn!("embedding failed (storing without vector): {e}");
}
```
**Verdict: Justified.** Embeddings are optional infrastructure. Memory is still stored and usable without a vector. This is by design.

### [justified] `src/tools/remember.rs:180-183` — supersession failure
```rust
Err(e) => {
    tracing::warn!(old_id = %old_id, "supersession failed: {e}");
}
```
**Verdict: Justified with caveat.** The new memory was already committed at this point. Propagating the error would return failure to the client even though the primary write succeeded. However: this silently creates an orphaned situation where the new memory exists but the old one was not superseded. The `check_unsuperseded_updates` alert exists to catch this, which is good mitigation. **Caveat:** the `tracing::warn!` here doesn't record the new memory ID, only the old one — makes it harder to correlate in logs.

### [justified] `src/tools/remember.rs:189-191` — vector store failure
```rust
if let Err(e) = crate::db::embeddings::store_vector_pub(db, id.as_str(), vector).await {
    tracing::warn!(id = %id, "failed to store embedding: {e}");
}
```
**Verdict: Justified.** Same rationale as embedding failure — vector is supplemental to the record.

### [justified] `src/tools/remember.rs:208-227` — chunking failure in spawned task
```rust
tokio::spawn(async move {
    match memory::get(&chunk_db, &chunk_parent_id).await {
        Ok(parent) => {
            if let Err(e) = chunking::chunk_and_embed(...).await {
                tracing::warn!(id = %chunk_parent_id, "chunking failed: {e}");
            }
        }
        Err(e) => {
            tracing::warn!(id = %chunk_parent_id, "failed to fetch for chunking: {e}");
        }
    }
});
```
**Verdict: Justified.** Background post-processing. Chunking is a performance optimization. JoinHandle is intentionally dropped.

### [justified] `src/db/auto_link.rs:148-153` (entity matching) and `157-168` (similarity linking) — in spawned task
```rust
tokio::spawn(async move {
    match match_known_entities(...).await {
        Err(e) => warn!(id = %memory_id, "entity matching failed: {e}"),
        ...
    }
    match auto_link_similar(...).await {
        Err(e) => warn!(id = %memory_id, "auto-link similar failed: {e}"),
        ...
    }
});
```
**Verdict: Justified.** Graph enrichment is a best-effort post-processing step. The primary memory write already succeeded.

### [unjustified] `src/db/search.rs:80-82` — SM-2 batch update uses `let _ =`
```rust
tokio::spawn(async move {
    let _ = db.query(&batch_sql).await;
});
```
**Verdict: Unjustified.** The `let _ =` pattern completely discards both the `Result` and any embedded SurrealQL errors from the batch. If SM-2 updates are silently failing (e.g., from format string issues in the inline SQL), there is no log trace at all. This is one of the only places in the codebase with zero error visibility on a DB write. Should at minimum be `if let Err(e) = ... { warn!(...) }`.

**Additional issue:** The batch SQL is built by string interpolation with `format!()` including user-derived field values (`u.id`, `u.ease_factor`, `u.review_interval`, `u.next_review_at`). The `id` value comes from a DB row (internal data), so injection risk is low, but this is the only query in the codebase not using parameterized bindings. The existing CLAUDE.md rule "All SurrealQL uses parameterized bindings — no string interpolation" is violated here.

### [justified] `src/db/search.rs:568-578` — access count update fire-and-forget
```rust
tokio::spawn(async move {
    let _ = db_clone.query("UPDATE memory SET ...").bind(...).await;
});
```
**Verdict: Justified in spirit, but same problem as SM-2 above** — errors are completely invisible. The `let _` should be replaced with a warn log so that systematic DB failures during recall don't go undetected.

### [justified] `src/db/search.rs:404-414` — vector search failure falls back to BM25
```rust
Err(e) => {
    tracing::warn!("vector search failed, using BM25 only: {e}");
    vec![]
}
```
**Verdict: Justified.** Graceful degradation to BM25-only is the right behavior for a best-effort vector layer.

### [justified] `src/db/search.rs:548-559` — spreading activation failure
```rust
Err(e) => {
    tracing::warn!("spreading activation failed: {e}");
}
```
**Verdict: Justified.** Spreading activation is an optional enrichment step; missing it doesn't break the primary results.

### [justified] `src/db/chunking.rs:121-123` — chunk-to-parent link failure
```rust
if let Err(e) = graph::link_memories(db, &chunk_id, parent_id, RelationType::Extends).await {
    warn!(chunk = i + 1, "failed to link chunk to parent: {e}");
}
```
**Verdict: Justified.** Chunk sub-memories are created and usable even if the `extends` edge fails. The loss is semantic richness, not data integrity.

### [justified] `src/db/auto_link.rs:63-64` — individual edge creation failure during auto-link
```rust
Err(e) => warn!(from = %memory_id, to = %similar_id, "auto-link failed: {e}"),
```
**Verdict: Justified.** Per-edge failures during auto-linking should not abort the whole operation.

### [justified] `src/db/markdown.rs:53` — alert check failure in markdown generation
```rust
let alert_list = alerts::check_all(db, alert_config).await.unwrap_or_default();
```
**Verdict: Justified.** The markdown file is still useful without the alerts section. However, using `unwrap_or_default()` rather than logging the error means alert failures are completely invisible during markdown regeneration. A `unwrap_or_else(|e| { warn!(...); vec![] })` would be better.

### [justified] `src/main.rs:61-63` — initial pruning failure on startup
```rust
if let Err(e) = kb_server::db::prune::run_all(&db, &config.pruning).await {
    tracing::warn!("initial pruning failed: {e}");
}
```
**Verdict: Justified.** Pruning failure should not prevent the server from starting.

### [justified] `src/main.rs:76-78` — periodic pruning failure in background loop
```rust
if let Err(e) = kb_server::db::prune::run_all(&prune_db, &prune_config).await {
    tracing::warn!("periodic pruning failed: {e}");
}
```
**Verdict: Justified.** Same as above — background maintenance should not crash the server.

---

## Client-Facing Error Messages

All tool handlers share this error serialization pattern:
```rust
Err(e) => format!("{{\"error\": \"{}\"}}", e)
```

`e` is a `KbError`. The `Display` impl (via `thiserror`) renders the full variant message. Analysis by variant:

### `KbError::Database { context, source }`
**Format:** `"database error: {context} — {source}"`

- `context` is always a hard-coded static string or format with IDs only (e.g., `"creating memory mem_abc123"`, `"BM25 recall query"`). No user content appears in context strings.
- `source` is `surrealdb::Error`. SurrealDB error messages can include:
  - Table/field names: e.g., `"The field 'summary' does not exist in the table 'memory'"`
  - Query fragments in some error types from the SurrealDB SDK
  - **Crucially:** In the current schema (SCHEMAFULL tables), SurrealDB errors should not echo back field values. The risk is low.
  - **However**, if a SurrealDB error were ever triggered by a query that includes a bound value in its text representation, that value could leak. This is an implementation detail of the SurrealDB error type outside our control.

**Risk level: Low-medium.** SurrealDB errors reveal the DB schema structure and table/field names to the MCP client. This is an implementation leak, not a content leak, and is acceptable for a local personal server. It would be a concern if exposed to untrusted clients.

### `KbError::NotFound(String)`
**Format:** `"memory not found: {0}"`

The string is always a memory ID (e.g., `"mem_abc123"`) or entity name. No content is leaked. **Risk level: None.**

### `KbError::InvalidInput(String)`
**Format:** `"invalid input: {0}"`

This is where privacy exposure is most likely:
- `src/db/memory.rs:270`: `"parsing memory {id}: {e}"` — contains a serde error, which may include field names and partial JSON. Could expose DB field names.
- `src/db/entity.rs:331`: `"parsing entity '{fallback_id}': {e}"` — same issue.
- `src/tools/remember.rs:101-103`: `"duplicate memory detected (similarity {similarity:.2}): existing memory:{existing_id}"` — **exposes an existing memory ID in the error message sent to the client.** This is intentional (the client needs to know which memory is duplicated) but should be documented as a design choice.
- Validation messages (summary length, importance bounds) are safe — they contain only the server's own rule descriptions, not user content.

**Risk level: Low.** Serde error messages in `InvalidInput` can reveal JSON field names (schema structure), not content. The duplicate detection message intentionally reveals a memory ID.

### `KbError::Embedding(String)`
**Format:** `"embedding error: {0}"`

Contains internal messages like `"model lock poisoned: ..."`, `"blocking task failed: ..."`, `"failed to load embedding model: ..."`. Could contain fastembed library error details, which may include file system paths to the ONNX model cache.

**Risk level: Medium.** The fastembed cache is in the user's home directory. An error during model loading could produce a message like `"failed to load embedding model: IO error: /home/user/.cache/fastembed/..."`, exposing a local file path. For a personal local server this is probably acceptable, but worth noting.

### `KbError::Io(std::io::Error)`
**Format:** `"io error: {0}"`

The `std::io::Error` `Display` output includes the OS error message and, in some cases when produced from path operations, the file path itself. Examples:
- `export_all` creates files: could expose `ExportResult.path` (the full absolute path of the export file).
- `queue.rs` opens `queue_dir/pending.jsonl`: could expose the queue directory path.
- `regenerate_file` writes to `output_path`: could expose the CLAUDE.md absolute path.

**Risk level: Medium.** File paths are internal implementation details. For a personal local server this is benign, but these paths are sent verbatim to the MCP client on IO errors. The `ExportResult` type actually also returns the full path in its success response, so the path disclosure is already intentional in the success case.

### `KbError::Config(String)`
**Format:** `"config error: {0}"`

Used correctly for configuration errors. The `llm/client.rs` misuse (documented below) could expose an LLM provider error message. **Risk level: Low.**

### `KbError::Json(serde_json::Error)`
**Format:** `"json error: {0}"`

serde_json errors include field path context in some cases (e.g., `"missing field 'summary' at line 1 column 42"`). No user content. **Risk level: None.**

### `kb_reembed` handler — `.unwrap()` on success path
```rust
Ok(result) => serde_json::to_string(&result).unwrap(),
```
`src/server.rs:272` uses `.unwrap()` instead of the `unwrap_or_else` pattern used everywhere else. If `ReembedResult` serialization ever fails (theoretically impossible for this struct, but still), this panics. **All other tool handlers use `unwrap_or_else`.**

---

## Silent Failures

### UPDATE queries that match 0 rows

**`src/db/memory.rs:172-187` — `promote()`**

```rust
db.query("UPDATE type::record('memory', $id) SET expires_at = NONE, ...")
    .bind(("id", id.as_str().to_string()))
    ...
    .await
    .map_err(|e| KbError::db(...))?;
Ok(())
```

The `promote()` function first calls `get()` to retrieve the memory (which will return `NotFound` if the ID doesn't exist), then issues an UPDATE. If the memory exists but the UPDATE matches 0 rows (e.g., due to a race condition or type mismatch), the function returns `Ok(())` silently. In practice, since `get()` returns `NotFound` for missing IDs and the UPDATE targets the same ID, this race window is tiny. Low risk but technically a silent success.

**`src/db/memory.rs:112-121` — `get_and_touch()` access_count increment**

```rust
db.query("UPDATE type::record('memory', $id) SET access_count += 1, ...")
    .await
    .map_err(|e| KbError::db(...))?;

get(db, id).await
```

If the UPDATE matches 0 rows (ID doesn't exist), no error is returned from the UPDATE itself. The subsequent `get()` call will correctly return `NotFound`. The silent UPDATE is harmless since the correct error is still raised. No real issue.

**`src/db/memory.rs:176-186` — `promote()` UPDATE (also above)**

Same pattern.

### RELATE queries where edge may not be created

**`src/db/graph.rs:44-45` and `88-90` — response statements are discarded with `unwrap_or_default()`**

```rust
let _: Vec<serde_json::Value> = response.take(0usize).unwrap_or_default();
let _: Vec<serde_json::Value> = response.take(1usize).unwrap_or_default();
```

The multi-statement queries (`LET $existing = ...; IF ... { RELATE ... }`) are executed and the results discarded. If the RELATE was silently skipped (entity not found: `$ent[0] = NONE`) or if the IF branch didn't execute, no error is surfaced. The `link_entity` and `link_assigned` functions can return `Ok(())` even when no edge was created (e.g., if the entity upsert succeeded but the lookup in the IF block returned NONE). This is the correct behavior for idempotent edge creation, but the caller has no way to distinguish "edge created" from "edge already existed" from "entity not found, edge skipped."

**Assessment:** This is by design (idempotent linking), but the silent skip case (entity name not found in the IF block) could hide a bug if the entity upsert is supposed to guarantee the entity exists before the RELATE.

### Background tokio::spawn tasks with dropped JoinHandles

All background tasks have their JoinHandles dropped:

1. **`src/db/search.rs:67-82`** — SM-2 updates (`let _ = db.query(...)`) — no error visibility at all
2. **`src/db/search.rs:569-578`** — access count updates (`let _ = db_clone.query(...)`) — no error visibility
3. **`src/tools/remember.rs:208-227`** — chunking — errors are logged inside the task, JoinHandle dropped
4. **`src/db/auto_link.rs:146-172`** — entity matching + similarity linking — errors are logged inside
5. **`src/db/embeddings.rs:131-143`** — `spawn_embed_and_store` — errors are logged inside

Tasks 3-5 log errors internally before the handle is dropped, so failures are visible in logs even though they're not propagated. Tasks 1 and 2 are the problematic ones — see "Swallowed Errors" section above.

---

## Error Type Misuse

### `KbError::Config` used for LLM runtime errors — `src/llm/client.rs:108`
```rust
return Err(KbError::Config(format!("All LLM providers failed: {e}")));
```
This is misuse. A runtime failure of an LLM API call is not a configuration error. It should be a new variant or at minimum `KbError::Embedding` (LLM inference is closer to embedding than config). The variant choice causes misleading error messages: the client sees `"config error: All LLM providers failed: ..."`, which implies a setup problem when it's actually a transient runtime failure.

**Impact:** The `llm/` module is not currently wired into any live tool handler (no tool calls `LlmClient::prompt` in the active code path), so this misuse has no current user-visible impact. When LLM features are activated, this will need fixing.

### `KbError::InvalidInput` used for DB deserialization errors
In two places, a serde deserialization failure on a DB row is surfaced as `InvalidInput`:
- `src/db/memory.rs:269-271`: `KbError::InvalidInput(format!("parsing memory {id}: {e}"))`
- `src/db/entity.rs:331`: `KbError::InvalidInput(format!("parsing entity '{fallback_id}': {e}"))`

These are not user input errors — they're DB schema/data inconsistencies. They should be `KbError::Database` (with a serde source converted to string). Using `InvalidInput` causes the client to see `"invalid input: parsing memory ..."`, which implies the client sent bad data when the actual problem is a DB row that doesn't deserialize cleanly (likely a schema migration gap or corrupt data).

### `KbError::NotFound` used for non-record-lookup parse failures
```rust
// src/db/memory.rs:223
_ => return Err(KbError::NotFound(id.to_string())),
// src/db/entity.rs:311
_ => return Err(KbError::NotFound(fallback_id.to_string())),
```
When a DB row comes back as a non-Object JSON value, the code returns `NotFound`. This is incorrect — the record was found (the DB returned something), but the shape is wrong. This should be `InvalidInput` or a new `Database` context error. However, this case would only occur in practice due to a serious DB schema corruption.

---

## Missing Error Context

### Bare `.await?` or `.take(0).unwrap_or_default()` without context strings

The following use `.unwrap_or_default()` on `Response::take()` calls, silently returning empty results rather than propagating deserialization errors:

| Location | Pattern | Risk |
|---|---|---|
| `src/db/search.rs:155` | `result.take(0).unwrap_or_default()` in spreading activation neighbor query | Silently returns no neighbors on deserialization error |
| `src/db/search.rs:227` | `result.take(0).unwrap_or_default()` in spreading activation fetch | Same |
| `src/db/search.rs:406` | `res.take(0).unwrap_or_default()` in vector search | Silently returns empty vector results |
| `src/db/search.rs:530` | `res.take(0).unwrap_or_default()` in entity-anchored extra fetch | Silently returns no extra results |
| `src/db/consolidation.rs:55` | `result.take(0).unwrap_or_default()` | Silently returns no candidates on error |
| `src/db/consolidation.rs:94` | `result.take(0).unwrap_or_default()` | Same |
| `src/db/reembed.rs:54` | `result.take(0).unwrap_or_default()` | Silently returns empty batch (loop terminates) |
| `src/db/prune.rs:72,168,198` | `result.take(0).unwrap_or_default()` | Silently reports 0 rows pruned on error |
| `src/db/markdown.rs:162` | `result.take(0).unwrap_or_default()` | Silently generates empty markdown section |
| `src/db/search.rs:719-723` | `result.take(N).unwrap_or_default()` ×5 in status_query | Status queries silently return empty on any deserialization error |

The pattern `result.take(0).unwrap_or_default()` is used for "best-effort" secondary queries throughout. In primary data paths (direct memory/entity fetches), `.map_err(...)` is used correctly. The split is intentional but not consistent — some secondary paths that should probably propagate errors (e.g., status query sub-results, reembed batch fetch) are silently swallowed.

### Generic or missing context strings

Several `KbError::db()` context strings are generic and reduce debuggability:

- `src/db/search.rs:355` — `"BM25 recall query"` — acceptable
- `src/db/search.rs:444,455` — `"entity-anchored query"` / `"person-anchored query"` — acceptable
- `src/db/search.rs:717` — `"status query"` — the combined 5-statement query; no indication of which sub-statement failed
- `src/db/markdown.rs:161` — `"markdown query"` — no indication of which of the 4 queries failed
- `src/db/consolidation.rs:53` — `"consolidation: project+subtype clusters"` — acceptable

The `markdown.rs` case is notable because `query_rows()` is called 4 times with the same context string `"markdown query"`. If one fails, the logged error won't indicate which query it was.

### `config.rs` — `MarkdownConfig::resolved_path()` uses `.unwrap()` on HOME
```rust
return std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(rest);
```
This panics if `HOME` is unset, even though the code checks `std::env::var("HOME").is_ok()` on the line above. The check and the unwrap are separate calls — a TOCTOU risk if the env is modified between them. In practice this cannot happen in a single-threaded env-read, but the pattern is fragile. Should use `std::env::var("HOME").unwrap_or_else(|_| ...)`.

---

## Recommendations

### P0 — Functional correctness

1. **`src/db/search.rs:80` — SM-2 `let _ =` must log errors.**
   The SM-2 batch update query uses string interpolation (violating the parameterized-bindings rule) and swallows all errors silently. Fix both:
   - Rewrite using parameterized bindings per memory in a loop, or use a SurrealDB transaction with proper bound parameters.
   - Replace `let _ = db.query(&batch_sql).await` with `if let Err(e) = ... { warn!(...) }`.

2. **`src/db/search.rs:572` — access count `let _ =` must log errors.**
   Same issue — `let _ = db_clone.query(...).await` discards all errors. Replace with a `warn!` on error.

3. **`src/server.rs:272` — `kb_reembed` handler uses `.unwrap()` not `unwrap_or_else`.**
   Inconsistent with every other handler. Change to `unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}}\""))`.

### P1 — Error type correctness

4. **`src/llm/client.rs:108` — `KbError::Config` misused for runtime LLM failure.**
   Add a `KbError::Llm(String)` variant, or use `KbError::Embedding` as a close approximation.

5. **`src/db/memory.rs:269`, `src/db/entity.rs:331` — `KbError::InvalidInput` misused for DB parse failures.**
   These should be `KbError::Database` (with the serde error stringified as context). The client currently sees `"invalid input: parsing memory ..."` which is misleading.

### P2 — Privacy / information disclosure

6. **`KbError::Embedding` may expose ONNX model cache file paths.**
   The fastembed model load error is passed verbatim to the client. Consider wrapping: `KbError::Embedding("model initialization failed (check server logs)".to_string())` for the load failure case, logging the full path only to stderr.

7. **`KbError::Io` may expose internal file paths.**
   Queue directory path, export directory path, and CLAUDE.md path can appear in IO error messages. For a personal local-only server this is acceptable. If this ever moves to a shared/network deployment, IO errors need sanitization.

### P3 — Observability improvements

8. **`src/db/markdown.rs:53` — alerts failure in `generate_summary` is invisible.**
   Replace `alerts::check_all(...).await.unwrap_or_default()` with `unwrap_or_else(|e| { tracing::warn!("alert check failed during markdown generation: {e}"); vec![] })`.

9. **`src/tools/remember.rs:180` — supersession warn lacks the new memory ID.**
   The warning logs `old_id` but not the newly created `id`. The new ID is in scope. Add it for log correlation.

10. **`src/db/search.rs` status_query `unwrap_or_default()` chain.**
    The 5-statement status query uses `unwrap_or_default()` for all result sets. If any sub-query fails silently (deserialization error), the status report returns partial/empty data with no indication of the problem. At least the top-level `.await?` propagates DB errors; only result deserialization is swallowed.

11. **`src/db/markdown.rs` — `query_rows()` context string `"markdown query"` used for all 4 queries.**
    Pass the query description as a parameter so errors identify which query failed.

### P4 — Architecture note

12. **`KbError` has no `Llm` variant and no `Unauthorized`/`Forbidden` variant.**
    As the LLM subsystem is activated and if auth is ever added to the HTTP transport, new variants will be needed. The current set is appropriate for the current scope.
