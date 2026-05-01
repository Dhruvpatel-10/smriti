# Security Audit

**Date:** 2026-04-10
**Scope:** Full source audit of smriti (personal knowledge base MCP server)
**Auditor:** Claude Opus 4.6 (1M context)

---

## Threat Model

The server is local-first, running as an MCP server backed by SurrealDB. Relevant attackers:

1. **Malicious MCP client** — any process that can send MCP tool calls (stdio or HTTP `127.0.0.1:47821`).
2. **Local filesystem attacker** — process with read access can read the SurrealDB data directory, config files, export files, and the generated CLAUDE.md. DB is unencrypted on disk.
3. **Network attacker (HTTP mode)** — server binds to `127.0.0.1:47821` with no auth. Any local process can issue requests.
4. **Supply chain** — crates.io and HuggingFace Hub. No model integrity verification beyond fastembed internals.

---

## Summary

Parameterized SurrealQL bindings used consistently, enum types prevent raw string injection, visibility filtering applied across all query paths. All critical, high, and medium findings are now closed.

**Findings by severity (as of 2026-04-21):**
- Critical: 2 (2 closed via ADR-009)
- High: 3 (3 closed via ADR-009)
- Medium: 7 (7 closed via ADR-009)
- Low: 5 (5 closed)
- Info: 4 (open — no action required)

---

## Findings

### [CLOSED] filesystem — tools/export.rs:33-36 — Arbitrary file write via kb_export path parameter
**Closed 2026-04-20 — `src/db/path_safety.rs::safe_filename()` called at export.rs:34-36 before any filesystem access; rejects empty names, names starting with `.`, and paths containing `/`, `\`, `..`, or null bytes.**

### [CLOSED] filesystem — tools/export.rs:58-63 — Arbitrary file read via kb_import path parameter
**Closed 2026-04-20 — `safe_filename()` called before opening import file; both import and export validate that filenames cannot escape the configured export directory.**

### [CLOSED] visibility-bypass — db/entity.rs:261-301 — Entity recent_memories leaks private/sensitive memories
**Closed 2026-04-20 — `AND visibility = 'standard'` filter verified at entity.rs:274 in the `recent_memories()` query.**

### [CLOSED] visibility-bypass — db/alerts.rs:26-67 — Alerts leak sensitive memory summaries
**Closed 2026-04-20 — All three alert functions now filter visibility: `check_overdue_actions` at line 32, `check_expiring_memories` at line 80, `check_unsuperseded_updates` at lines 179-180.**

### [CLOSED] visibility-bypass — db/alerts.rs:167-216 — Contradiction alerts leak sensitive memory summaries
**Closed 2026-04-20 — Covered by the alerts.rs fix above; `check_unsuperseded_updates` now includes `AND out.visibility = 'standard' AND in.visibility = 'standard'` at lines 179-180.**

### [CLOSED via ADR-009] visibility-bypass — db/search.rs:209-216 — Spreading activation leaks private memories cross-project
**Closed 2026-04-21 — ADR-009 Fix 4: kb_drill now enforces the same visibility semantics as recall; spreading activation unchanged. See ADR-009 for details.**

### [CLOSED] visibility-bypass — db/consolidation.rs:38-55 — Consolidation candidates leak sensitive memory summaries
**Closed 2026-04-20 — Both heuristics now use `visibility = 'standard'`; consolidation candidates have no project-scope parameter so cross-project standard-only is correct.**

### [CLOSED] visibility-bypass — db/search.rs:662-696 — Status query allows private cross-project
**Closed 2026-04-20 — `status_query` now applies the two-branch filter matching the recall contract; the total count sub-query also applies `status = 'active' {visibility_filter}`. (ADR-011 2026-04-21 subsequently swept all read predicates to `BITEMPORAL_ACTIVE`.)**

### [CLOSED via ADR-009] injection — server.rs error responses — JSON injection in error messages
**Closed 2026-04-21 — ADR-009 Fix 1: All ~22 tool handler sites now use `serde_json::json!({...}).to_string()` instead of unescaped format string interpolation. Verified by json-escape fuzzer test covering `"`, `\n`, `\\` injection attempts. See tests/tool_tests.rs.**

### [CLOSED] privacy-logging — db/alerts.rs:53-63 — Summary content in alert messages exposed to MPC client
**Closed 2026-04-20 — Accepted by design. All alert queries gate on `visibility = 'standard'` so only standard-visibility summaries reach alert messages. Inline comments document the decision. Single-user local-first; alerts must carry summaries to be actionable.**

### [CLOSED] denial-of-service — tools/remember.rs — No size limit on content field
**Closed 2026-04-20 — `MAX_CONTENT_BYTES = 500 * 1024` added to `src/db/validation.rs`; content validated before create; three unit tests cover at-limit, over-limit, and no-content cases.**

### [CLOSED] denial-of-service — db/auto_link.rs:95-130 — Entity name scan is O(entities × summary_length)
**Closed 2026-04-20 — `MAX_CANDIDATES = 64` cap added with warn-on-truncate; FTS index `idx_entity_name_ft` added in migration 005; `match_known_entities` now uses `@@` operator instead of full table scan.**

### [CLOSED 2026-04-20] data-integrity — tools/remember.rs:164-179 — Supersession race: atomic check missing
**Closed 2026-04-20 — `src/db/memory.rs:159-183` now uses `WHERE status = 'active'` + `RETURN BEFORE` for atomic conditional update; the second concurrent call sees the memory already superseded and receives an error before any new record is written. (ADR-011 2026-04-21 updated guard to `WHERE invalidated_at IS NONE`.)**

### [CLOSED 2026-04-20] injection — db/graph.rs:32-35 — Table name interpolated into SurrealQL from enum
**Closed 2026-04-20 — Accepted. Table names come from `RelationType` enum variants (not user input); `RelationType::Display` now delegates to `as_str()` which is a compile-time `match` — infallible, no empty-string risk. No user input reaches SQL text.**

### [CLOSED] authentication — main.rs:162 — HTTP transport has no authentication
**Closed 2026-04-20 — Accepted by design per ADR-002 local-first; localhost binding is the intended trust boundary.**

### [CLOSED] filesystem — config.rs:153-157 — Queue directory from environment variable without validation
**Closed 2026-04-20 — `queue_dir()` now returns `Result` and validates directory existence.**

### [CLOSED] filesystem — db/markdown.rs:129-148 — Markdown output path writes to arbitrary location
**Closed 2026-04-20 — Server-controlled path; comment added to markdown.rs documenting the intentional design. Path is set in config, not from user tool input.**

### [CLOSED] privacy-logging — db/alerts.rs:53-56 — Alert messages contain memory summary text
**Closed 2026-04-20 — Accepted by design. All alert queries gate on `visibility = 'standard'`; server logs only `count = alerts.len()` (no content). Inline comments document the decision.**

### [CLOSED] denial-of-service — tools/queue.rs:30-33 — Queue size check is TOCTOU race
**Closed 2026-04-20 — Bounded read via `.take()` now enforces size limit atomically.**

---

## Auto-consolidation Privacy Notes (ADR-026)

### [INFO] privacy — `consolidation_attempt.source_ids` stores RecordIds
`source_ids` is an `array<record<memory>>` — it stores SurrealDB RecordIds (not summaries or
content). A direct DB dump reveals which memory IDs were co-evaluated in a cluster, but not
their content. Trust boundary: the DB file is local-only, same access level as the memories
themselves. No additional exposure beyond the existing DB-read threat model (see threat model
above, attacker class 2).

### [INFO] privacy — Merged memory inherits max-strictness visibility from sources
`consolidate_atomic` sets the merged memory's visibility to the most restrictive value
across all source memories (e.g., if any source is `Private`, the merged memory is `Private`;
if any source is `Sensitive`, the merge is blocked before the sidecar call).
`Sensitive` memories are rejected by `fetch_source_metadata` before being sent to the
sidecar — they never reach the LLM.

### [INFO] privacy — `synthesize_consolidation` sends summaries to Claude API
The `synthesize_consolidation` sidecar call sends memory summaries (not content) to the
Claude API via the existing Max subscription OAuth path. Sensitive memories are excluded
before the sidecar call (defended in `fetch_source_metadata` with `WHERE invalidated_at IS NONE`
and a `Visibility::Sensitive` check). This is the same defense baseline as
`extract_relations` and `extract_anchors`. Summary-only input (no content field) is
the prompt-injection defense.

---

## Info Findings (no action required)

### [INFO] injection — All SurrealQL queries use parameterized bindings
No SurrealQL injection vectors found. The only `format!` interpolations use static WHERE clause fragments, enum-derived table names, or static immune subtype lists.

### [INFO] data-integrity — kb_forget only soft-deletes
`memory::archive()` correctly sets `status = 'archived'`. No hard-delete function exists.

### [INFO] supply-chain — fastembed model downloads from HuggingFace Hub
Downloads over HTTPS; models cached locally. No model integrity hash verification beyond fastembed internals. ONNX Runtime is sandboxed — impact of a malicious model would be degraded embedding quality, not code execution.

### [INFO] supply-chain — LLM API keys handled correctly
API keys read from environment variables only, never from config files, never logged. Rig framework handles keys internally.

### [INFO] data-integrity — importance range not enforced by SurrealDB schema
`Importance` newtype validates 1-5 at construction time, but the SurrealDB schema has no CHECK constraint. Direct DB manipulation or a crafted import file could set importance outside 1-5. Requires direct DB or import-file access.
