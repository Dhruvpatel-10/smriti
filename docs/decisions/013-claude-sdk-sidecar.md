# ADR-013: Claude Agent SDK Sidecar for Relation Extraction

**Status:** Accepted  
**Date:** 2026-04-29  
**Supersedes:** ADR-008 §1 (Rig/Gemini/Groq LLM integration)

---

## Context

ADR-008 §1 specified rig-core (Rust) as the LLM layer, using Gemini Flash as the primary provider
and Groq as a fallback. That pipeline was never wired into production paths — `src/llm/client.rs`
and `src/llm/classify.rs` had zero callers, and the `rig-core` Cargo dependency was compile-time
only. The drain loop, background classification, and auto-importance-scoring from ADR-008 were
never built.

The concrete Phase 1 LLM task is narrow: **extract typed semantic relations between memories**
so that spreading activation can traverse meaningful edges instead of only cosine-similarity
edges. This is a post-write background task, not a hot path.

Two constraints shaped the replacement design:

1. **No API key.** The user is on Claude Max subscription, not an Anthropic API plan. Any
   provider that requires `ANTHROPIC_API_KEY` would incur additional cost on top of the existing
   subscription.
2. **SDK availability.** `@anthropic-ai/claude-agent-sdk` (Node.js) falls back to the Claude
   Code OAuth session when `ANTHROPIC_API_KEY` is absent. The Rust `rig-core` crate has no
   equivalent fallback path.

The clean answer is a TypeScript subprocess: sidecar receives extraction requests over JSON-RPC
2.0, calls the SDK, and returns structured relations. Rust never touches an HTTP client or an API
key.

---

## Decision

Run a Node.js process (`sidecar/src/index.ts`) as a long-lived child process managed by a Rust
supervisor (`src/llm/sidecar.rs`). Rust communicates with the sidecar over **JSON-RPC 2.0 on
stdin/stdout**. The sidecar exposes four methods: `health`, `extract_relations`,
`extract_anchors` (added with ADR-014), and `synthesize_consolidation` (added with ADR-026).

Key invariants:

- **API key stripped at both layers.** Rust removes it with `Command::env_remove("ANTHROPIC_API_KEY")`
  before spawn (`src/llm/sidecar.rs:275`); TypeScript deletes it at process entry
  (`delete process.env.ANTHROPIC_API_KEY` at top of `sidecar/src/index.ts`). Belt-and-suspenders.
- **`outputFormat: { type: "json_schema", schema: RELATION_SCHEMA }`.** The SDK enforces the
  shape; no fragile JSON.parse on free-form text.
- **Sensitive memories are never sent.** `src/tools/remember.rs` skips extraction for
  `Visibility::Sensitive` memories. This is the baseline prompt-injection defense; content is
  never included regardless of visibility.
- **Summary-only.** The `extract_relations` payload contains only the memory summary, not the
  content field.
- **Circuit breaker.** `SidecarClient` tracks `consecutive_failures`. After
  `max_consecutive_failures` (default 5), it stops spawning requests and logs a warning. Resets
  on the next successful response.
- **Sidecar failure does not block the server.** `KbServer` holds `Option<Arc<SidecarClient>>`.
  If the sidecar fails to start, or if `llm.enabled = false`, the server boots normally and
  extraction is silently skipped.
- **Post-create async spawn.** Extraction is kicked off in a tokio task after `kb_remember`
  returns. It is never on the caller's critical path.

Relations are persisted as `typed_relation` edges (MIGRATION_012) with a `confidence` field and
`retracted_at` for soft-deletion. Spreading activation (`src/db/search.rs::spreading_activation`)
traverses these edges with `retracted_at IS NONE AND confidence >= 0.5`.

---

## Alternatives Considered

- **rig-core (Rust-native LLM crate)** — Does not integrate with Claude Code OAuth session;
  requires `ANTHROPIC_API_KEY`. Rejected for the same reason the existing rig-core dependency
  was removed: incompatible with Max subscription auth path.

- **Ollama (local LLM)** — No API key required, but extraction quality is significantly lower
  than a frontier model for relation inference. User explicitly chose Claude Max-quality
  extraction over the convenience of a fully local stack.

- **In-process via PyO3 or similar** — Embedding a Python runtime in the Rust process to call
  the Anthropic SDK was considered. Rejected: same crash-coupling risk that subprocess isolation
  is designed to avoid; binary size and startup time increase substantially; no benefit over the
  subprocess approach.

- **Caller-side extraction (have the calling LLM extract)** — MCP callers are Claude sessions
  that already have Max access. They could call `kb_link` directly with typed relations. Rejected:
  this puts extraction quality entirely in the hands of the calling LLM's prompt, makes testing
  impossible without a live session, and violates the server-enforces-quality invariant
  established in the project's design principles.

---

## Consequences

### Positive

- Zero additional API cost — extraction runs on the existing Max subscription.
- Subprocess isolation — a sidecar crash does not affect the Rust server; the supervisor
  restarts it with exponential backoff (cap 60 s).
- `outputFormat` json_schema enforcement — malformed relation shapes are rejected at the SDK
  layer, not in application code.
- Kill switch is one config field: `llm.enabled = false`. No schema change, no migration.
- Spreading activation gains semantic edges in addition to cosine-similarity edges,
  improving recall diversity without changing the retrieval API.

### Negative

- **Node runtime dependency.** `node` is now a runtime requirement. Documented alongside
  SurrealDB in setup instructions.
- **Best-effort extraction.** Relations can be missing (low confidence filtered at 0.7 for
  persistence) or wrong (confidence-floor at 0.5 in spreading activation). The knowledge graph
  is opportunistic, not complete.
- **No unit-testable real-LLM path.** Integration tests use a JS mock at
  `tests/fixtures/mock_sidecar.js`. Real-LLM behavior is verified via the bench suite
  (`bench/`), not the integration test suite.
- **Latency: ~1–3 s per `extract_relations` call.** Mitigated entirely by async post-create
  spawn — callers never wait.
- **Two runtimes to maintain.** TypeScript sidecar has its own `package.json`, build step
  (`tsc`), and release artifact (`sidecar/dist/index.js`).

### Trade-offs Accepted

- Extraction is skipped for `Visibility::Sensitive` memories permanently. Sensitive memories
  will have no `typed_relation` edges and receive no benefit from sidecar-derived spreading
  activation. This is a deliberate privacy preference.
- Confidence floor in spreading activation (0.5) is lower than persistence threshold (0.7).
  Low-confidence edges that survived retraction still participate in traversal — intentional,
  as weak associations can still surface relevant memories.

---

## Implementation Map

| File | Role |
|------|------|
| `sidecar/src/index.ts` | JSON-RPC 2.0 server; `health` + `extract_relations` + `extract_anchors` (ADR-014) + `synthesize_consolidation` (ADR-026) methods; `query()` call with `outputFormat`, `permissionMode: "dontAsk"`, `allowedTools: []`, `maxBudgetUsd: 0.05`, `model: "claude-sonnet-4-5"`; 25–30 s `AbortController` per request (30 s for synthesize_consolidation) |
| `src/llm/sidecar.rs` | `SidecarClient`; supervisor loop with exponential backoff (cap 60 s); circuit breaker (`max_consecutive_failures`); reader/writer task pair; `parking_lot::Mutex<HashMap<id, oneshot>>` pending map; `RpcError` thiserror enum |
| `src/llm/extractor.rs` | `extract_and_persist`; validates from == submitted memory_id, confidence ≥ threshold, kind parses to `RelationKind` enum, target memory exists; all failures counted in `ExtractStats` |
| `src/llm/mod.rs` | Module root; `LlmConfig` re-export |
| `src/db/typed_relation.rs` | Persistence + retract; idempotent on `(in, out, kind)` |
| `src/db/migrations.rs` | MIGRATION_012 — `typed_relation` edge table; MIGRATION_013 — entity `archived_at` for `kb_entity_delete` |
| `src/db/search.rs` | `spreading_activation` traverses `typed_relation` with `retracted_at IS NONE AND confidence >= 0.5` |
| `src/tools/remember.rs` | Post-create spawn calls `extract_and_persist`; guarded by `Visibility::Sensitive` check |
| `src/server.rs` | Holds `Option<Arc<SidecarClient>>`; sidecar init failure is non-fatal |
| `src/config.rs` | `LlmConfig` struct (see Operational Knobs below) |
| `tests/fixtures/mock_sidecar.js` | Deterministic JS mock for integration tests |

---

## Operational Knobs (`LlmConfig`)

| Field | Default | Purpose |
|-------|---------|---------|
| `enabled` | `false` | Kill switch — set to `true` to activate sidecar |
| `sidecar_path` | `sidecar/dist/index.js` | Path to compiled sidecar entry point |
| `confidence_threshold` | `0.7` | Minimum confidence for relation persistence |
| `request_timeout_ms` | `30000` | Per-request timeout (sidecar uses 25 s AbortController; Rust adds 5 s margin) |
| `max_consecutive_failures` | `5` | Circuit-breaker threshold |

The spreading-activation confidence floor (0.5) is a compile-time constant in
`src/db/search.rs`, not a config knob. It applies to traversal of already-persisted edges.

---

## New MCP Tools

- `kb_relation_retract(id)` — soft-retract a `typed_relation` edge by setting `retracted_at`.
  Retracted edges are excluded from spreading activation but preserved for audit.
- `kb_entity_delete(id)` — soft-delete an entity by setting `archived_at` (MIGRATION_013).
  Replaces the previous lack of any entity deletion path.

---

## Rollback Plan

Set `llm.enabled = false` in `config.toml` and restart the server.

- The sidecar process will not be spawned.
- `typed_relation` edges already written to the DB remain intact and continue to function in
  spreading activation.
- No migration rollback is required — MIGRATION_012/013 are additive; the tables are simply not
  populated when the sidecar is disabled.
- The `rig-core` dependency is not restored. If a different LLM provider becomes necessary,
  implement a new sidecar or replace the TypeScript sidecar with a different subprocess.

---

## References

- ADR-008: LLM Integration, Server-Side Intelligence — §1 superseded; §§2–9 (taxonomy, write
  path, retrieval, decay) remain canonical
- ADR-011: Bitemporal Memory Model — `typed_relation` edges are not bitemporal (Phase 2 edge
  timestamps explicitly rejected); retraction uses `retracted_at` following the same
  soft-invalidation pattern
- ADR-014: Write-time anchor extraction — added `extract_anchors` sidecar method
- ADR-026: Auto-consolidation — added `synthesize_consolidation` sidecar method; second
  isolated `SidecarClient` instance for the periodic loop
- MIGRATION_012 — `typed_relation` edge table schema
- MIGRATION_013 — entity `archived_at` field
- MIGRATION_018 — `consolidation_attempt` ledger table (ADR-026)
- `CLAUDE.md` — write path invariants, `RelationKind` enum, spreading activation cap
