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

Every MCP tool handler in `src/server.rs` serializes errors as:
```rust
Err(e) => format!("{{\"error\": \"{}\"}}", e)
```

This calls `Display` on `KbError` with no sanitization layer between internal errors and the MCP client response. For a local single-user server this is acceptable; would be a concern if exposed to untrusted clients.

**All actionable findings closed as of 2026-04-20.**

---

## Swallowed Errors — Audit Rubric

When reviewing swallowed errors, check whether the failure is:
1. In a best-effort post-processing step (dedup, embedding, chunking, auto-link) → justified to swallow; must log at WARN.
2. In a primary data path (memory create, entity upsert) → must propagate.
3. In a background fire-and-forget task → must log inside the task before the JoinHandle is dropped.

### Justified swallows (no action needed)

| Location | Why justified |
|---|---|
| `src/tools/remember.rs:110-113` — dedup check failure | Dedup is best-effort; fallback is to store without dedup |
| `src/tools/remember.rs:116-119` — embedding failure on write path | Embeddings are optional; memory usable without vector |
| `src/tools/remember.rs:180-183` — supersession failure | New memory already committed; alert system catches orphaned supersessions |
| `src/tools/remember.rs:189-191` — vector store failure | Vector is supplemental; primary record already written |
| `src/tools/remember.rs:208-227` — chunking failure in spawned task | Background post-processing; errors logged inside task |
| `src/db/auto_link.rs:148-168` — entity matching + similarity in spawned task | Graph enrichment is best-effort; logged inside task |
| `src/db/search.rs:404-414` — vector search falls back to BM25 | Graceful degradation; BM25 still runs |
| `src/db/search.rs:548-559` — spreading activation failure | Optional enrichment; logged at WARN |
| `src/db/chunking.rs:121-123` — chunk-to-parent link failure | Chunk sub-memories usable even without the extends edge |
| `src/db/auto_link.rs:63-64` — individual edge creation failure | Per-edge failures should not abort the whole auto-link run |
| `src/main.rs:61-63, 76-78` — initial and periodic pruning failure | Pruning failure must not prevent the server from starting or running |

### [CLOSED] `src/db/search.rs:80-82` — SM-2 batch update uses `let _ =`
**Closed 2026-04-20 — Replaced with parameterized SurrealDB FOR loop (`"FOR $u IN $updates { UPDATE ... }"`) and proper `tracing::warn!` on error. `Sm2Update` struct derives `serde::Serialize` to enable the `.bind()` bridge.**

### [CLOSED] `src/db/search.rs:568-578` — access count update fire-and-forget
**Closed 2026-04-20 — Verified current code uses `tracing::warn!` on error; original audit misread the code.**

### [CLOSED] `src/server.rs:272` — `kb_reembed` handler uses `.unwrap()`
**Closed 2026-04-20 — Handler now uses `unwrap_or_else` consistent with all other tool handlers.**

---

## Client-Facing Error Messages — Privacy Notes

| Variant | Risk | Notes |
|---|---|---|
| `Database { context, source }` | Low-medium | SurrealDB errors reveal schema structure (table/field names), not content values. Acceptable for local personal server. |
| `NotFound(String)` | None | Always a memory ID or entity name; no content leaked. |
| `InvalidInput(String)` | Low | Serde errors may reveal JSON field names (schema, not content). Duplicate detection intentionally reveals an existing memory ID. |
| `Embedding(String)` | Medium | May include ONNX cache file paths (fixed Wave C — load failure now wraps error without exposing full path). |
| `Io(std::io::Error)` | Medium | Accepted by design for personal local-only server. File paths sent verbatim to MCP client on IO errors. |
| `Config(String)` | Low | Used correctly for configuration errors. |
| `Json(serde_json::Error)` | None | Field path context only, no user content. |

---

## Error Type Correctness

### [CLOSED] `src/llm/client.rs:108` — `KbError::Config` misused for runtime LLM failure
**Closed 2026-04-20 — The `KbError::Llm` variant already exists and is used at line 108. Audit was written before the llm module was implemented; no code change required.**

### [CLOSED] `src/db/memory.rs:269`, `src/db/entity.rs:331` — `KbError::InvalidInput` misused for DB parse failures
**Closed 2026-04-20 — Both sites now return `KbError::Database` (not `InvalidInput`). DB deserialization failures are not user input errors.**

### [CLOSED] `KbError::NotFound` used for non-record-lookup parse failures
**Closed 2026-04-20 — Fixed alongside the InvalidInput misuse pass.**

---

## Silent Failures — Residual Notes

### UPDATE queries matching 0 rows
`promote()` and `get_and_touch()` pre-fetch via `get()` before issuing UPDATE. The narrow race window where a concurrent archive occurs between get and update is low risk — the next call will correctly return `NotFound`.

### RELATE queries where edge may not be created
`link_entity` and `link_assigned` return `Ok(())` even when no edge was created (idempotent by design). Callers cannot distinguish "edge created" from "edge already existed" from "entity not found, edge skipped." This is the intended contract for idempotent linking.

### Background tokio::spawn tasks with dropped JoinHandles
Tasks 3-5 (chunking, auto-link, embed-and-store) log errors internally before the handle is dropped — failures are visible in logs. SM-2 and access count tasks are also logged (see closed findings above).

---

## Observability

### [CLOSED] `src/db/markdown.rs:53` — alerts failure in `generate_summary` is invisible
**Closed 2026-04-20 — `unwrap_or_default()` replaced with `unwrap_or_else(|e| { tracing::warn!("alert check failed during markdown generation: {e}"); vec![] })`.**

### [CLOSED] `src/tools/remember.rs:180` — supersession warn lacks the new memory ID
**Closed 2026-04-20 — Added `new_summary_len`, `memory_type`, and `subtype` to all three rollback warn! calls. No content logged — only length and enum variants.**

### [CLOSED] `src/db/search.rs` status_query `unwrap_or_default()` chain
**Closed 2026-04-20 — All five `.take(N)` calls in `status_query` use `.map_err(|e| KbError::db(...))` with distinct context strings per statement.**

### [CLOSED] `src/db/markdown.rs` — `query_rows()` context string `"markdown query"` used for all 4 queries
**Closed 2026-04-20 — Each of the four call sites now passes a distinct context string.**

---

## Architecture Note

`KbError` has no `Unauthorized`/`Forbidden` variant. When LLM features are activated and if auth is ever added to the HTTP transport, new variants will be needed. The current set is appropriate for the current scope.
