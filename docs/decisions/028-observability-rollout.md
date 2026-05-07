# ADR-028: Observability Rollout (tracing-journald, request_id, recall histograms, sd-notify)

**Date:** 2026-05-01
**Status:** Accepted

## Context

smriti runs as a long-lived systemd daemon shared across multiple users. Prior to this
rollout, observability gaps made production diagnosis difficult:

- Log format was plain-text only; no structured JSON for log aggregators and no
  journald-native output for `journalctl -k` filtering.
- Tool calls had no correlation identifier; a request spanning post-create async tasks
  could not be traced end-to-end.
- Retrieval performance was invisible — no per-stage latency data, so regressions in BM25,
  vector, rerank, or spreading activation were undetectable without custom instrumentation.
- systemd had no health signal beyond the process being alive; `WATCHDOG=1` was unused,
  so `WatchdogSec` in the unit could not be set meaningfully.
- High-frequency tracing targets (sidecar supervisor retries) could flood the log stream
  with suppressed/repeated events, masking actionable signals.
- The `kb_status` tool returned only operational data; no diagnostics block for embedder
  state, sidecar circuit state, DB record counts, or recall percentiles.

## Decision

Ship 7 observability steps as a single rollout. All steps landed in code on 2026-05-01.

### Step 1 — tracing-journald + log format toggle

`init_tracing()` in `src/main.rs` builds a layered `tracing_subscriber::registry()`.
`[observability].log_format` selects the fmt layer:
- `journald` — `tracing_journald` layer; default when systemd is detected (auto-detect via
  `JOURNAL_STREAM` env var).
- `json` — JSON structured output to stderr.
- `pretty` — human-readable colored output; default when systemd is not detected.

`EnvFilter` reads `RUST_LOG`; defaults to `info`.

### Step 2 — request_id propagation

New `RequestId(String)` newtype in `src/types/ids.rs` using UUIDv7 (time-ordered).
`tokio::task_local! REQUEST_ID` carries the ID within a tool call's async scope.
`pub fn current_request_id() -> Option<RequestId>` is the accessor.

The `tool_call!` macro in `src/server.rs` wraps every tool handler body in:
```
REQUEST_ID.scope(rid, async move { ... }).instrument(span).await
```
Span name: `mcp_tool` with fields `tool` and `request_id`. The 3 special handlers
(`kb_regenerate_md`, `kb_consolidation_candidates`, `kb_reembed`) use the same
scope+instrument pattern directly.

Background tasks spawned post-create do NOT inherit `request_id` — they use
`operation_id = memory_id` for their own correlation. This is documented at
`src/types/ids.rs:115-121`.

### Step 3 — sidecar liveness

Private `enum State { Healthy | BackingOff { until, fail_count } | Broken }` drives the
supervisor state machine. Public projection `pub enum CircuitState { Healthy | BackingOff
| Broken }` (snake_case serde) is exposed via `pub fn circuit_state() -> CircuitState`.

`last_success_at_ms: Arc<AtomicU64>` is updated only on `Ok(_)` from `call()`.
`pub fn last_success_age() -> Option<Duration>` computes elapsed time since last success.
These are consumed by the `kb_status` diagnostics block (Step 5).

### Step 4 — hdrhistogram recall-stage percentiles

New `src/db/recall_metrics.rs` module:
- `pub enum RecallStage { Bm25, Vector, Fusion, Rerank, SpreadingActivation }`
- 5 `Mutex<Histogram<u64>>` per stage (1µs–60s range, 3 significant figures).
- `init()`, `record(stage, duration)`, `snapshot_and_reset() -> RecallStagePercentiles`.
- `RecallStagePercentiles` (Serialize/Deserialize/JsonSchema): p50/p95/p99/max in µs per
  stage.

All 5 stages in `src/db/search.rs` are instrumented with `Instant::now()` +
`recall_metrics::record(...)` bookends. Vector stage records only when `query_vector` is
`Some`.

Cargo dependency added: `hdrhistogram = { version = "7.5", default-features = false }`.

### Step 5 — kb_status diagnostics block

`StatusInput` gained `include_diagnostics: Option<bool>`.
`StatusReport` gained `diagnostics: Option<DiagnosticsBlock>` (serde `skip_serializing_if = "Option::is_none"`).

`DiagnosticsBlock` fields:
- `version: String`
- `uptime_secs: u64`
- `embedder: EmbedderDiagnostics { provider, model, dimensions }`
- `sidecar: SidecarDiagnostics { circuit_state: String, restart_count, dropped_inflight, last_success_age_secs: Option<u64> }`
- `db: DbDiagnostics { migrations_applied, total_memories, active_memories, total_entities, total_agents }`
- `recall_stage_p95: RecallStagePercentiles`

`tools::status::execute` takes 3 args: `(db, ctx, input)` where `ctx: DiagnosticsCtx<'a>
{ embedder, sidecar }`. Startup instant recorded in `src/observability/mod.rs` via
`OnceLock`; `init()` + `uptime() -> Option<Duration>` exposed from that module.

### Step 6 — ThrottleLayer + crash skiplist

New `src/observability/throttle.rs` implements a `tracing_subscriber::Layer` that drops
repeated events from the same callsite within a configurable window.

- `ThrottleConfig { window: Duration, allow_target_prefixes: Vec<String> }`
- Default window: 500 ms.
- Default allowlist prefixes: `["sidecar", "smriti::llm"]` — these targets always pass
  through unthrottled.
- Bypass also applies when the event has a field `event_class` containing `"crash"` or
  `"panic"`.
- Atomic `suppressed` counter exposed via `suppressed_handle()` for inclusion in
  `kb_status` diagnostics.
- Layer is stacked before the fmt layer in `init_tracing()` registry.

### Step 7 — sd-notify + sidecar RPC meta

`notify_systemd_ready(tasks)` in `src/main.rs`:
- Emits `READY=1` via `sd_notify` after listener bind in `run_http()`.
- Spawns a Watchdog ticker at `window/2` interval (where `window` = `WatchdogUSec` from
  environment), wired to `TaskRegistry` for clean shutdown.

`JsonRpcRequest` envelope in `src/llm/sidecar.rs` gained optional `meta: Option<RpcMeta>`
(skip_if_none). `RpcMeta { request_id: String }` is populated from
`current_request_id()` inside `call()`, so sidecar logs can correlate with server-side
spans using a single `request_id`.

Version negotiation was **descoped** from this rollout. It would require TS sidecar
changes and falls outside the scope of server-side observability. It remains a future
candidate.

Cargo dependency added: `sd-notify = "0.5"`.

## Consequences

### Positive
- Full request tracing: UUIDv7 `request_id` in every MCP tool span + sidecar RPC envelope.
- journald-native logging when running under systemd; structured JSON available for
  aggregators.
- Per-stage recall latency percentiles (p50/p95/p99/max) visible in `kb_status` response.
- `WatchdogSec` in the systemd unit is now effective; process health surfaced to the
  service manager.
- High-frequency sidecar retry events throttled by default (500 ms window); crash-class
  events always pass through.
- `kb_status(include_diagnostics=true)` exposes embedder, sidecar circuit state, DB
  record counts, and uptime without requiring log access.

### Negative
- `tools::status::execute` signature changed from 2-arg to 3-arg (`DiagnosticsCtx`
  added); callers updated.
- `hdrhistogram` and `sd-notify` added to dependency tree.
- `ThrottleLayer` may suppress non-sidecar warnings if they repeat faster than 500 ms;
  operators can widen the allowlist in config if needed.

### Trade-offs accepted
- Version negotiation with the TS sidecar descoped; sidecar must remain compatible at the
  protocol level without a formal version handshake until a future ADR addresses it.
- Background task correlation uses `operation_id = memory_id` (not `request_id`); the
  spawning tool call's `request_id` is not propagated across the `tokio::spawn` boundary.

## Alternatives considered

- **OpenTelemetry full stack** — rejected for this rollout. OTLP export, a collector
  sidecar, and a tracing backend are out of scope for a single-node personal server.
  `tracing` spans remain compatible with OTel if added later.
- **Per-request histogram reset** — rejected. A shared histogram with `snapshot_and_reset`
  on each `kb_status` poll gives rolling percentiles without per-request allocation.
- **Propagate request_id across tokio::spawn** — rejected. `task_local!` does not cross
  spawn boundaries by design. Post-create tasks are fire-and-forget; correlating them to
  the originating request via `memory_id` is sufficient and simpler.

## Related

- ADR-002: HTTP daemon transport (systemd deployment context)
- ADR-013: Claude Agent SDK sidecar (supervisor and circuit breaker extended by Step 3)
- ADR-022: Adaptive reranker policy (rerank stage now measured by Step 4)
- ADR-023: Weighted spreading activation (spreading activation stage now measured by Step 4)
- ADR-026: Auto-consolidation (second SidecarClient; circuit-breaker state surfaced by Step 5)
