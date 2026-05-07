# Logging & Observability Audit

**Date:** 2026-04-10
**Scope:** All `src/` files
**Auditor:** Claude Sonnet 4.6 (automated)

---

## Current Setup (updated 2026-05-01 — ADR-028 landed)

**Subscriber:** `tracing_subscriber::registry()` with layered fmt layer. Format selected
by `[observability].log_format`: `journald` (default under systemd via `JOURNAL_STREAM`),
`json`, or `pretty`. ThrottleLayer stacked before fmt layer.
**Default log level:** `INFO`. Controlled via `RUST_LOG` env var; falls back to `EnvFilter::new("info")` if unset.
**Output:** All logging to `stderr`. Correct — stdout is reserved for MCP protocol.
**No log rotation or file sink.** journald handles retention when running under systemd.
**`#[instrument]` coverage:** All public async functions in `src/db/` and all tool execute paths carry `#[instrument]` (Wave F). Safe fields (type, subtype, project) captured; content/summary/query skipped.
**Request correlation:** UUIDv7 `RequestId` task-local per tool call; propagated to sidecar RPC via `meta.request_id` in `JsonRpcRequest`. Background tasks use `operation_id = memory_id`.
**ThrottleLayer:** Per-callsite drop within 500 ms window; sidecar/llm targets and crash-class events bypass throttle.
**sd-notify:** `READY=1` emitted after listener bind; Watchdog ticker at `WatchdogUSec/2` interval.

---

## Privacy Violations — Audit Rubric

**Rule:** Never log memory content (summary, content, query text). Never log real person names to the tracing stream. Entity IDs (opaque UUIDs) and enum classifiers (kind, type, subtype) are safe.

### Confirmed violations (all closed)

### [CLOSED] `src/db/auto_link.rs:109` — entity name in warning
**Closed 2026-04-20 — Entity name removed from error context strings in graph.rs lines 69, 85, 114, 127. The `entity_name` parameter is still bound into SurrealQL via `.bind()` (safe), but error messages no longer include it.**

### [CLOSED] `src/db/entity.rs:20` — `#[instrument]` with `fields(name = %name)`
**Closed 2026-04-20 — `name` is now skipped; span only captures `kind = %kind`. All error-context `format!` strings in graph.rs that interpolated `entity_name` have been redacted.**

### Residual boundary cases (accepted)

**`src/db/alerts.rs` — summaries in alert messages:** Alert messages contain raw memory summaries (`"Action item overdue: {summary}"`). These are not emitted to the tracing stream — only `count = alerts.len()` is logged. Alerts are returned to the MCP client and appear in the generated CLAUDE.md. All alert queries gate on `visibility = 'standard'`; inline comments document the design. Accepted for single-user local-first use.

**`src/db/entity.rs:133` — entity name in error string:** `KbError::db(format!("getting entity by name '{name}'"), e)` includes the raw name if this error propagates upstream to the tracing stream. Low risk for a personal server; noted for awareness.

**`src/main.rs:56`, `src/db/export.rs:83-88`, `src/db/export.rs:103` — file paths in logs:** Export/import paths logged at INFO. Reveals KB data directory layout to anyone reading logs. Acceptable for personal use.

---

## Missing Observability — Findings and Status

### [CLOSED] Startup banner
**Closed 2026-04-20 — `main.rs` logs structured INFO event with `server_version`, transport mode, DB path, and config path at startup.**

### [CLOSED] Memory count at startup
**Closed 2026-04-20 — "smriti server ready" INFO event includes `active_memories` and `total_memories` fields via `memory::count()` at `src/main.rs:65-75`. Degrades to 0 on query failure so startup never blocks.**

### [CLOSED] Tool call tracing
**Closed 2026-04-20 — All 17 tool execute functions carry `#[instrument]` with safe span fields (memory_type, subtype, project, limit); content/summary/query/query_vector skipped (Wave F).**

### [CLOSED] Dedup rejections
**Closed 2026-04-20 — Dedup events logged at INFO with similarity score and matched ID on rejection; near-duplicate warnings also logged at INFO (Wave C).**

### [CLOSED] Supersession events
**Closed 2026-04-20 — Supersession events logged with old_id and new_id.**

### [CLOSED] Entity name removed from `#[instrument]` span
**Closed 2026-04-20 — `entity::upsert` now skips `name`.**

### [CLOSED] Entity name removed from WARN
**Closed 2026-04-20 — `auto_link::match_known_entities` error context redacted.**

### [CLOSED] project/type/limit fields added to `search::recall` span
**Closed 2026-04-20 — Added in Wave F.**

### [CLOSED] Embeddings ERROR downgraded to WARN
**Closed 2026-04-20 — Model load failure now logged at WARN; server continues without vectors (graceful degrade).**

### [CLOSED] Auto-link results
**Partially closed 2026-04-20 — `tracing::debug!` added for auto-link complete event (Wave A). Aggregate per-session log absent but low priority.**

### [CLOSED] No per-request correlation identifier
**Closed 2026-05-01 (ADR-028 Step 2) — UUIDv7 `RequestId` task-local propagated to every tool call span and sidecar RPC `meta.request_id` envelope.**

### [CLOSED] No structured log format for systemd/aggregators
**Closed 2026-05-01 (ADR-028 Step 1) — `[observability].log_format` toggle; journald default under systemd, json available for aggregators.**

### [CLOSED] High-frequency sidecar log flooding
**Closed 2026-05-01 (ADR-028 Step 6) — ThrottleLayer drops repeated same-callsite events within 500 ms window; sidecar/llm targets exempt.**

### [CLOSED] No liveness health signal to systemd
**Closed 2026-05-01 (ADR-028 Step 7) — sd-notify `READY=1` on bind; Watchdog ticker at `WatchdogUSec/2`.**

### [CLOSED] Recall latency invisible
**Closed 2026-05-01 (ADR-028 Step 4) — hdrhistogram per-stage (BM25/Vector/Fusion/Rerank/SpreadingActivation); p50/p95/p99/max µs in `kb_status` diagnostics.**

### [CLOSED] No diagnostics in kb_status
**Closed 2026-05-01 (ADR-028 Step 5) — `kb_status(include_diagnostics=true)` returns DiagnosticsBlock with embedder, sidecar circuit state, DB counts, recall percentiles, uptime.**

### Still open (low priority, no action required currently)
- **Supersession logged but not auditable:** When `supersede()` returns `Ok(true)`, there's no INFO log of "memory X superseded by memory Y" for audit purposes. WARN log exists for failures.
- **Embedding model load duration** not logged.
- **HTTP transport: no per-request logging** — debug-only connection logging at `tracing::debug!(%addr, "new connection")`.
- **Config validation:** Config load success emits nothing — no "loaded config from /path/config.toml" message.
- **Queue operations:** `src/tools/queue.rs` has no logging. Queue writes and size-limit rejections invisible in log stream.
- **`#[instrument]` span names** not explicitly set to match MCP tool names (e.g., `name = "kb_remember"`).
- **`src/db/search.rs:557` — spreading activation failure** logged at WARN; arguably DEBUG since it's a best-effort enhancement.
- **Version negotiation with sidecar** descoped from ADR-028; no formal protocol version handshake between Rust server and TS sidecar.

---

## Log Level Contract

| Level | Contains |
|---|---|
| ERROR | Unrecoverable failures: DB connection failed, config file missing/invalid (process must exit) |
| WARN | Degraded functionality: embedding failed, dedup failed, vector search failed, LLM provider down, queue full, supersession rollback |
| INFO | Operational events: server started, memory count at startup, migration outcomes, pruning results, export/import complete, markdown regenerated |
| DEBUG | Per-request details: tool call params (type/subtype/project, NOT content), dedup result, supersession events, auto-link counts, spreading activation results |
| TRACE | Verbose internals: SQL queries and timing, SM-2 computation, chunk creation, per-row parse results |

### How to enable debug mode

```bash
# All debug output from smriti only
RUST_LOG=smriti=debug ./smriti

# Debug plus surrealdb query noise
RUST_LOG=smriti=debug,surrealdb=info ./smriti

# Full trace for a specific module
RUST_LOG=smriti::db::search=trace ./smriti
```

For HTTP daemon (systemd):
```ini
[Service]
Environment=RUST_LOG=smriti=info
StandardError=journal
```
