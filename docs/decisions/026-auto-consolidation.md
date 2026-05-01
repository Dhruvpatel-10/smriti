# ADR-026: Server-Side Auto-Consolidation Loop

**Date:** 2026-04-30
**Status:** Proposed
**Extends:** ADR-013 (Claude SDK sidecar), ADR-002 (HTTP daemon transport), ADR-011 (bitemporal memory)

## Context

Manual `kb_consolidate` requires the user (or smriti-librarian) to identify clusters,
write a merged summary, and call the tool. Most users never do this. Memories accumulate;
redundant clusters bloat recall results and dilute spreading activation scores.

The sidecar introduced in ADR-013 is already capable of LLM-driven synthesis —
`extract_relations` and `extract_anchors` demonstrated the pattern. A periodic
consolidation pass using the same subprocess model can merge clusters automatically,
with strong guardrails to prevent unsafe or costly merges.

The existing 24h periodic loop in `src/main.rs` (governed by `[pruning].consolidation_interval_hours`)
already calls `find_candidates` and logs results. The missing piece is: call the sidecar
for each cluster and merge when the LLM endorses it.

## Decision

Add a server-side periodic auto-consolidation loop, gated off by default
(`[consolidation].auto_enabled = false`). When enabled, the existing 24h loop calls a
new sidecar method `synthesize_consolidation` for each candidate cluster and invokes
`consolidate_atomic` only when the LLM returns `should_merge = true && confidence >=
min_confidence`. Every attempt is recorded in a new `consolidation_attempt` ledger table
with a configurable cooldown to prevent re-evaluation cost on perpetually-deferred
clusters.

## Implementation

### Wave 1 — Prerequisite fixes (bundled)

- `find_candidates` now accepts `min_age_days: Option<u32>` and `max_cluster_size:
  Option<u32>` filters. GROUP BY adds `author` for per-author scoping.
- Heuristic 2 (graph traversal) filters neighbor visibility to `Standard` — prevents
  `Sensitive` memories from anchoring clusters.
- `consolidate_atomic` now sets `valid_until = time::now()` AND `invalidated_at =
  time::now()` on source memories when archiving them. Previously only `invalidated_at`
  was set — a latent bitemporal bug that allowed `kb_recall_at(as_of=T_before_merge)`
  to miss the originals.
- `consolidate_atomic` now aggregates metadata from source memories: `max(importance)`,
  `union(people, tags, domain)`, `max-strictness(visibility)`. Previously it used
  first-source-only values.
- New table `consolidation_attempt` (migration 018) with UNIQUE index on `cluster_hash`.
- New module `src/db/consolidation_ledger.rs`.

### Wave 2 — Sidecar method + config plumbing

New sidecar JSON-RPC method `synthesize_consolidation`:

- Input: `{cluster_id, summaries: [string], subtype, project?, domain?}`
- Output: `{should_merge: bool, confidence: float, merged_summary?: string, reason: string}`
- Budget: `maxBudgetUsd: 0.05` per call, 30 s `AbortController`.
- SDK `outputFormat: { type: "json_schema" }` — structured response, no free-form parsing.
- Content (full memory body) is never sent — summary-only (same prompt-injection defense
  as `extract_relations` and `extract_anchors`).

New `[consolidation]` config section (see Config section below).

The auto-consolidation loop uses a **second `SidecarClient` instance**, isolated from
the `extract_relations`/`extract_anchors` hot path. A circuit-breaker trip on the
consolidation sidecar does not disable extraction on `kb_remember`.

### Wave 3 — Loop wiring in `src/main.rs`

Per tick (every `consolidation_interval_hours`), when `auto_enabled = true`:

1. Call `find_candidates(db, max_per_tick * 3, Some(min_age_days), Some(max_cluster_size))`
   — author-scoped; neighbor-visibility filtered.
2. For each candidate cluster:
   a. Compute `cluster_hash` = SHA-256 of sorted source IDs (stable, order-independent).
   b. Look up `consolidation_attempt` by `cluster_hash`; skip if `skip_until > now()`.
   c. Re-fetch source memory summaries. Reject if any source has `Visibility::Sensitive`.
   d. Call `synthesize_consolidation(cluster_id, summaries, subtype, project, domain)`.
   e. If `should_merge && confidence >= min_confidence`: call `consolidate_atomic` with
      `source_agent = "auto-consolidator"`, then trigger async `kb_regenerate_md`.
   f. Record attempt in `consolidation_attempt` ledger with outcome and `skip_until =
      now() + cooldown_days` regardless of outcome.
3. Stop after processing `max_per_tick` clusters per tick.

The `auto-consolidator` agent (kind: `Summarizer`) is registered at server startup when
`auto_enabled = true`, using `kb_agent_register` semantics (resurrection-safe).

### New table: `consolidation_attempt` (migration 018)

| Field | Type | Description |
|-------|------|-------------|
| `cluster_hash` | `string` | SHA-256 of sorted source IDs; UNIQUE index |
| `source_ids` | `array<record<memory>>` | Source memory record links |
| `attempted_at` | `datetime` | Timestamp of last attempt |
| `last_outcome` | `string` | `Merged` \| `DeferredLowConfidence` \| `DeferredOversized` \| `DeferredSensitive` \| `SidecarError` \| `MergeFailed` |
| `last_reason` | `option<string>` | LLM-supplied reason (or error string) |
| `merged_into` | `option<record<memory>>` | Set on `Merged` outcome; FK to result memory |
| `skip_until` | `datetime` | Cooldown fence; loop skips cluster until this passes |
| `attempt_count` | `int` | Cumulative attempts on this cluster hash |

### File layout

```
src/db/consolidation_ledger.rs   — new: attempt upsert, skip-until check, outcome types
src/db/consolidation.rs          — modified: find_candidates signature, consolidate_atomic fixes
src/main.rs                      — modified: loop wiring, SidecarClient instance management
sidecar/src/index.ts             — modified: synthesize_consolidation method
src/config.rs                    — modified: ConsolidationConfig struct
config.toml                      — modified: [consolidation] section
```

## Config

All knobs live under `[consolidation]` in `config.toml`:

| Key | Default | Description |
|-----|---------|-------------|
| `auto_enabled` | `false` | Enable the auto-consolidation loop |
| `min_age_days` | `7` | Minimum age (days) of source memories before candidacy |
| `min_confidence` | `0.75` | LLM confidence threshold to trigger a merge |
| `max_per_tick` | `5` | Max clusters merged per periodic tick |
| `max_cluster_size` | `12` | Reject clusters with more source memories than this |
| `cooldown_days` | `7` | Days before re-evaluating a previously-deferred cluster |

Tick frequency is governed by the existing `[pruning].consolidation_interval_hours` (default `24`) — not duplicated under `[consolidation]`.

## Rationale

- **Per-author scoping** (`find_candidates` GROUP BY `author`): prevents cross-tenant
  merge on the shared multi-user daemon (ADR-002). Each author's memories only cluster
  with their own.
- **Isolated `SidecarClient`**: consolidation circuit-breaker is independent from the
  `kb_remember` post-create extraction path. A bad consolidation run cannot disable
  relation extraction for new memories.
- **Cooldown ledger**: prevents perpetual cost burn on clusters the LLM keeps deferring.
  One Sonnet call per cluster per `cooldown_days` maximum.
- **LLM confidence + reason**: every `consolidation_attempt` row is an auditable
  decision record — operators can inspect why a cluster was or was not merged.
- **Bitemporal `valid_until`** on archived sources: `kb_recall_at(as_of=T_before_merge)`
  surfaces the original memories by respecting the `valid_until <= T_before_merge`
  boundary. Historical state is preserved. (Wave 1 fix — was a latent bug.)
- **`derives` graph edges**: `consolidate_atomic` already wires `derives` edges from
  the merged memory to each source. `kb_drill(merged_id)` exposes the full provenance
  chain even after sources are archived.

## Tradeoffs

- **LLM summary distortion**: `merged_summary` is LLM-generated and may paraphrase or
  reframe source content. Mitigation: originals remain in DB (never hard-deleted),
  reachable via `derives` edges through `kb_drill(merged_id)`, and via `kb_recall_at`
  for pre-merge state.
- **Cost per tick**: each candidate cluster = one Sonnet call (`maxBudgetUsd = 0.05`).
  With `max_per_tick = 5`, the maximum per-tick spend is $0.25. Cooldown caps recurrent
  cost per cluster to once per 7 days.
- **Author scoping gap**: `find_candidates` scopes by `memory.author` (the `AgentId`
  FK). User-authored memories (`author = NULL`) all bucket into the same author group.
  On a shared multi-user daemon (stark + leanscale), user-authored memories from both
  users CAN cluster together if they share a `project` string. The two `auto-consolidator`
  agent instances would not cross because the cooldown is per `cluster_hash` (which
  includes source IDs from both users). Mitigation: use distinct `project` strings per
  user. This is a known limitation; a proper `owner_user_id` column (per the ADR-010
  future-migration note) is the long-term fix.
- **UNIQUE-index race on `consolidation_attempt`**: concurrent ticks (extremely unlikely
  given the hour-scale interval) could race on `cluster_hash` upsert. Impl-reviewer
  follow-up #2 — low risk, document only.
- **`fetch_source_metadata` BITEMPORAL_ACTIVE gap**: the inner re-fetch of source
  memories before sidecar call did not originally apply the `BITEMPORAL_ACTIVE`
  predicate, allowing already-archived sources to be sent to the sidecar. Fixed in the
  polish pass — `fetch_source_metadata` now uses `WHERE invalidated_at IS NONE`.
  Impl-reviewer follow-up #3 closed.

## Consequences

### Positive
- Redundant memory clusters are automatically merged on a 24h cadence when enabled.
- Every merge decision is auditable via the `consolidation_attempt` table.
- Manual `kb_consolidate` contract is unchanged — existing workflows unaffected.
- `kb_recall_at` historical correctness improved by the bitemporal fix (Wave 1) bundled here.
- `consolidate_atomic` now produces richer merged memories (max importance, union people/tags/domain).

### Negative — must accept
- New infra surface: `consolidation_attempt` table, second `SidecarClient`, new config section.
- LLM cost when enabled (bounded by cooldown + `max_per_tick`).
- `merged_summary` quality depends on the sidecar model; LLM errors produce `SidecarError`
  outcome (no merge, cluster deferred for `cooldown_days`).

### Trade-offs accepted
- Gated OFF by default. No operator impact until explicitly enabled.
- Summary-only input to sidecar (no content) trades some merge quality for prompt-injection safety.

## Pilot plan

1. Ship gated `auto_enabled = false`. No behavior change from operator perspective.
2. After 1 week of stable daemon operation, set `auto_enabled = true` in
   `/etc/smriti/config.toml` and restart: `systemctl restart smriti`.
3. Watch `journalctl -u smriti -f` for `info!` "auto-consolidated cluster" log lines.
4. Inspect `consolidation_attempt` after first tick:
   ```sql
   SELECT * FROM consolidation_attempt ORDER BY attempted_at DESC LIMIT 20;
   ```
5. Promote ADR to Accepted after 7 days of clean operation.

## Alternatives considered

- **Drain-session consolidation (external LLM call from drain session)** — rejected.
  Drain session is optional for correctness (CLAUDE.md invariant); consolidation requiring
  it would make the feature unreliable. Server-side loop preserves the invariant.
- **Always-on (no gate)** — rejected. Auto-merge with LLM-generated summaries is a
  quality risk until the sidecar's `synthesize_consolidation` is validated in production.
  Gate-off-by-default is a safer rollout.
- **Single shared `SidecarClient`** — rejected. A circuit-breaker trip during
  consolidation would disable `extract_relations`/`extract_anchors` on every `kb_remember`.
  Isolation is mandatory.
- **Pure heuristic merge (no LLM)** — rejected. Cosine similarity alone cannot judge
  whether merging two memories produces a coherent, accurate summary. LLM endorsement
  + confidence score is the quality gate.

## Budget Cap

**Added:** 2026-05-01

### Problem

The original design only limited sidecar calls via `max_per_tick` (clusters per single
tick). On the first tick after a long pause, many unconsolidated clusters could appear
simultaneously, each triggering one Sonnet call. With `max_per_tick = 5` and a 24h
interval this is bounded — but if the interval is shortened or a large backlog appears,
cost could spike beyond operator expectations.

Additionally, a cooldown only prevents re-evaluation of *already-attempted* clusters.
Brand-new clusters always bypass the cooldown check and are evaluated unconditionally.

### Decision

Add a **rolling 24h budget cap**: `[consolidation].max_attempts_per_day` (default `20`).

Before every `synthesize_consolidation` call, the loop queries the `consolidation_attempt`
ledger for the count of attempts recorded in the last 24 hours (regardless of outcome):

```sql
SELECT count() AS n FROM consolidation_attempt
WHERE attempted_at >= $since GROUP ALL
```

If `n >= max_attempts_per_day`, the loop logs an `info!` message and breaks out of the
candidate iteration for the current tick. No further clusters are processed in that tick.

### Interaction with `max_per_tick`

Both limits apply:
- `max_per_tick` caps clusters merged *per tick* (short-term rate limiter).
- `max_attempts_per_day` caps total sidecar calls in a rolling 24h window (cost ceiling).

A single tick cannot exhaust the daily budget beyond `max_per_tick` per tick; the daily
cap is an additional safeguard when multiple ticks occur within 24h or when the tick
interval is reduced.

### Config

| Key | Default | Description |
|-----|---------|-------------|
| `max_attempts_per_day` | `20` | Max `synthesize_consolidation` calls across all ticks in any rolling 24h window |

### Implementation

- `src/db/consolidation_ledger.rs`: `count_attempts_last_24h(db) -> Result<u32>` —
  parameterized `SELECT count() GROUP ALL` query with `$since` binding.
- `src/main.rs`: guard inserted before every `synthesize_consolidation` call in the
  per-candidate loop; breaks the inner loop (skips remaining clusters) when cap hit.
- `src/config.rs`: `ConsolidationConfig.max_attempts_per_day: u32` with serde default.
- `src/llm/sidecar.rs`: `SidecarClient::new` now accepts a `CancellationToken`
  parameter. The supervisor loop respects cancellation on both backoff sleeps and the
  outer loop boundary, ensuring clean shutdown without re-spawn attempts during teardown.

## Related

- ADR-002: HTTP daemon transport (multi-user daemon context for author-scoping rationale)
- ADR-011: Bitemporal memory model (bitemporal `valid_until` fix on source archive)
- ADR-013: Claude Agent SDK sidecar (sidecar process model; `synthesize_consolidation` extends it)
- ADR-010: Agent authorship (`auto-consolidator` agent registration)
