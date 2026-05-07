# ADR-029: Spreading Activation Structural Hardening

**Date:** 2026-05-01
**Status:** Proposed
**Extends:** ADR-023 (weighted spreading activation), ADR-022 (adaptive reranker policy), ADR-028 (observability rollout)

## Context

ADR-023 closed the immediate problem of uniform edge traversal and left one item
explicitly open: "cap tuning empirical — calibration requires the ADR-021 eval set."

A multi-agent review (auditor + researcher + devils-advocate) concluded that grid-search
cap tuning on a personal corpus is structurally unsound for four independent reasons:

1. **Circular labels.** Any labeled query set assembled from the pipeline's own output
   reflects the pipeline's current biases. Ground-truth labels for cap calibration would
   need to come from an independent retrieval oracle — which smriti does not have at
   n=1 personal-corpus scale.

2. **Degenerate axes.** Spreading activation adds at most `cap` neighbours across all
   edge types. At personal-corpus scale (10k–100k memories), the walker fills ≤30% of
   final result slots. Optimising cap via grid-search is optimising the wrong stage for
   the dominant signal (BM25 + vector).

3. **Wrong failure metric.** Recall@10 does not detect hub-displacement failure: a
   super-hub entity may surface 3 low-relevance neighbours that displace the 3 highest-
   scoring non-hub neighbours. The failure is invisible to Recall@10 but damages result
   quality for hub-heavy queries.

4. **Published practice confirms hand-picking.** A structured literature review of
   2024–2026 personal-memory and graph-retrieval systems (HippoRAG, SA-RAG, Mem0, Letta,
   Zep) found no production system that learns spreading activation caps from data.
   All hand-pick. ADR-023 is at-or-ahead of published practice.

Grid-search tuning is therefore formally closed as a dead end. Two real failure modes
remain open:

- **Super-hub pollution at the `involves` traversal level.** Weight 0.3 (ADR-023)
  reduces per-neighbour score but does not prevent a node with degree 500 from
  contributing up to `cap=3` slots regardless of relevance.
- **Silent recall collapse on sidecar model swap.** `TYPED_RELATION_CONFIDENCE_FLOOR`
  is a file-local constant in `src/db/search.rs:256`. Swapping the sidecar model
  (ADR-016 / embedder-swap runbook) changes the confidence distribution without any
  config or diagnostic signal, silently degrading `typed_relation` edge propagation.

A third gap surfaced by the ADR-028 observability rollout: spreading activation stage
latency is now measured (Step 4), but internal walker behaviour — candidates seen,
neighbours added, cap saturation — is invisible. "0 eligible neighbours" is
indistinguishable from "37 discarded by cap=3" without walker-level counters.

## Decision

Replace the "tune via grid-search" path with six structural and observability
hardening changes. No grid-search. No schema change.

### 1 — Hub-degree cap on `involves` traversal

Skip any entity node on an `involves` edge whose degree exceeds
`search.spreading.involves_hub_cap` (default 50). The cap applies only to `involves`
edges; high-degree nodes reached via `derives`, `extends`, `updates`, or `relates` are
not affected.

```toml
[search.spreading]
involves_hub_cap = 50   # skip entity nodes with degree > this on involves edges
```

This is a structural fix: super-hub pollution is eliminated at the traversal source
rather than diluted by weight alone. An entity node with degree > 50 is by definition
a corpus-wide hub ("Claude", a project name, a frequent collaborator) whose neighbours
are essentially uncorrelated with the query.

### 2 — Surface `typed_relation_confidence_floor` to config

Move the file-local constant at `src/db/search.rs:256` to `EdgeWeightsConfig`:

```toml
[search.spreading.edge_weights]
typed_relation_confidence_floor = 0.5   # was hardcoded; edges below floor are skipped
```

Default value unchanged (0.5). Sidecar model swaps now have an explicit operator lever:
lower the floor if the new model's confidence distribution shifts downward; raise it for
a higher-precision model. Previously, a silent model swap caused recall collapse with no
observable or configurable remedy.

### 3 — Walker observability counters

Add `SpreadingActivationStats` accumulator to the walker:

```rust
pub struct SpreadingActivationStats {
    pub candidates_seen: usize,
    pub neighbors_added: usize,
    pub cap_saturated: bool,       // true if walker exited due to cap, not exhaustion
    pub hub_skipped: usize,        // involves nodes skipped by involves_hub_cap
}
```

Emit via `tracing::debug!` at walker exit. Aggregate into `recall_metrics` alongside
the per-stage latency percentiles introduced in ADR-028 Step 4.

`kb_status(include_diagnostics=true)` exposes rolling p95 `neighbors_added` and
`cap_saturated` rate so operators can detect chronic cap saturation without log access.

### 4 — Per-call `spreading: bool` override

Mirror the `rerank: Option<bool>` caller-override pattern from ADR-022:

```
kb_recall(query, ..., spreading?) -> [...]
```

`spreading=false` suppresses walker entirely for that call. The smriti-librarian
subagent uses this for direct-lookup queries where graph traversal adds noise (e.g.,
`kb_recall` immediately following `kb_drill` on a known ID).

Default behaviour is unchanged: `spreading` absent → apply `RerankerPolicy`-equivalent
logic (always walk unless corpus is tiny).

### 5 — Reflect SpreadingConfig in `kb_status` diagnostics

Extend `DiagnosticsBlock.spreading` (new field, `skip_if_none`) with the active
runtime values:

```json
"spreading": {
  "cap": 3,
  "hop_decay": 0.5,
  "involves_hub_cap": 50,
  "typed_relation_confidence_floor": 0.5,
  "edge_weights": { "derives": 1.0, "extends": 0.8, "updates": 0.8,
                    "relates": 0.5, "involves": 0.3, "typed_relation_max": 0.9 }
}
```

Operators running `kb_status(include_diagnostics=true)` see the active config without
inspecting the config file or restarting the server.

### 6 — Two ranking-order tests in `tests/search_retrieval.rs`

Add two tests that assert position, not just membership:

- **Hub-displacement test.** Inject a super-hub entity (degree > 50) connected to
  low-relevance memories and a high-relevance memory. Assert that the high-relevance
  memory ranks above the hub's neighbours in the final result set.
- **Cap-saturation test.** Inject a scenario where `cap=3` would be saturated by
  hub neighbours if `involves_hub_cap` were absent. Assert that with the cap active,
  the correct top-3 are returned.

These tests pin ranking order, not just set membership, catching hub-displacement
regressions that Recall@10 misses.

## Consequences

### Positive

- Super-hub pollution addressed structurally: `involves_hub_cap=50` prevents high-
  degree nodes from contributing to the walk regardless of per-edge weight. Combines
  with ADR-023's weight=0.3 for two independent suppression layers.
- Sidecar model swaps no longer cause silent recall collapse: `typed_relation_
  confidence_floor` is now a runtime-observable, operator-adjustable config field.
- Walker behaviour is observable: `SpreadingActivationStats` distinguishes "cap
  saturated by hub neighbours" from "walker found nothing" — previously
  indistinguishable.
- Per-call `spreading` override gives the librarian subagent a suppression knob for
  query patterns where graph traversal is known to add noise.
- `kb_status` diagnostics surface the active spreading config, closing the
  "what is the server actually doing" gap for operators.
- Ranking-order tests catch hub-displacement regressions before they reach production.

### Negative

- `involves_hub_cap` requires a degree lookup per `involves` edge traversal. One
  additional count query per involves-edge candidate. At graph sizes under 1M edges
  this is negligible; at larger scale, degree can be cached on the entity record.
- `kb_recall` gains a new optional parameter (`spreading`). Callers that pass a fixed
  parameter set are unaffected; the librarian prompt must be updated to document when
  to suppress.
- `DiagnosticsBlock` gains a new field. Serialized response size increases by ~200 bytes
  when `include_diagnostics=true`. No impact on non-diagnostic calls.

### Trade-offs accepted

- `involves_hub_cap=50` is a starting heuristic. The right value is corpus-dependent.
  With walker observability (Change 3) and ranking-order tests (Change 6), future
  adjustment is evidence-based rather than speculative.
- `SpreadingActivationStats` adds a small allocation per `kb_recall` call (usize
  counters). This is accepted to close the observability gap.
- Grid-search cap tuning is permanently closed as a path. Future behavioral-signal
  tuning (session logs of query → subsequent memory act as non-circular ground truth)
  is deferred until that signal accumulates at meaningful volume.

## Alternatives considered

- **Build labeled query set + grid-search** — rejected. Circular labels (see Context
  §1), wrong metric (Recall@10 misses hub-displacement failure, Context §3), degenerate
  axes (walker fills ≤30% of slots, Context §2). Structurally invalid regardless of
  corpus size.
- **Swap to Personalized PageRank** — rejected. ADR-023 already rejected PPR with
  three compounding problems (heterogeneous edges violate uniform random-walk
  assumption; super-hub accumulation; per-query BFS cost at 16M+ edges). No published
  head-to-head showing PPR wins over weighted local BFS at personal-corpus scale.
- **PolyG query-taxonomy routing** — deferred. PolyG (arxiv 2504.02112) routes
  queries through a taxonomy to select graph traversal strategy per query type.
  Promising, but requires corpus at least 10× current scale for the taxonomy to have
  discriminative power. Revisit when corpus exceeds 100k memories.
- **Do nothing** — rejected. Super-hub failure mode is real and unaddressed by
  weight=0.3 alone (a degree-500 node at weight 0.3 still contributes cap=3 slots).
  `typed_relation_confidence_floor` as a hidden constant is a known operational risk
  post ADR-016 cloud embedder. The observability gap blocks all future tuning decisions.

## References

- [ADR-022](022-adaptive-reranker-policy.md) — per-call `rerank` override pattern mirrored by Change 4
- [ADR-023](023-weighted-spreading-activation.md) — edge weight table and traversal cap this ADR extends
- [ADR-028](028-observability-rollout.md) — recall-stage percentiles and `kb_status` diagnostics this ADR extends

**Literature:**

- HippoRAG — arxiv 2405.14831. Graph-augmented RAG with Personalized PageRank;
  hub nodes handled by dampening, not learned caps. Confirms hand-tuning norm.
- SA-RAG — arxiv 2512.15922. Spreading-activation RAG; fixed activation threshold,
  no grid-search over cap values in production configuration.
- Mem0 — arxiv 2504.19413. Personal memory system; graph traversal depth set by
  heuristic, not learned from corpus labels.
- LoCoMo — arxiv 2402.17753. Long-context conversational memory benchmark; retrieval
  caps set per-system by authors, not tuned.
- PolyG — arxiv 2504.02112. Query-taxonomy-driven graph traversal routing; deferred
  (see Alternatives).
