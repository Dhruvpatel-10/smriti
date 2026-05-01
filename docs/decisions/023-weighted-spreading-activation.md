# ADR-023: Weighted Spreading Activation by Edge Type

**Status:** Proposed — 2026-04-30
**Date:** 2026-04-30
**Extends:** ADR-005 (hybrid retrieval), ADR-010 (agent authorship), ADR-013 (Claude SDK sidecar)

## Context

`spreading_activation()` in `src/db/search.rs` (currently around line 173) expands
the RRF result set by traversing graph edges from matched memories. The current
implementation:

- Treats all edge types with equal weight.
- Uses a hardcoded `cap=3` on the depth of traversal.
- Applies a fixed score fraction regardless of edge semantics.

The graph contains edges with radically different semantic weight:

| Edge type | Origin | Semantic strength |
|---|---|---|
| `derives` | LLM-extracted causal link (sidecar, ADR-013) | High — explicit causal provenance |
| `extends` | Explicit user link via `kb_link` | High — intentional connection |
| `updates` | Explicit supersede via `kb_remember(updates_id=...)` | High — version chain |
| `relates` | General link via `kb_link` | Medium — declared but unspecified |
| `involves` | Entity co-occurrence, auto-linked at ≥0.75 cosine | Low — fires promiscuously |
| `typed_relation` | Sidecar-extracted with confidence score (ADR-013) | Variable — confidence-gated |

The `involves` edge is the primary failure mode: an entity such as "Claude" or a
frequent project name may appear in hundreds of memories. Uniform spreading
activation lets these super-hubs propagate low-relevance neighbours into the result
set, polluting recall with tangentially related memories.

Personalized PageRank was evaluated as an alternative global graph scoring strategy
and rejected (see Alternatives).

## Decision

Replace uniform traversal with per-edge-type weight coefficients applied to
neighbour score:

```
neighbour_score = parent_score × edge_weight × hop_decay^hop_depth
```

### Edge weight table

| Edge type | Weight | Notes |
|---|---|---|
| `derives` | 1.0 | Full propagation — causal relevance |
| `extends` | 0.8 | Strong — intentional structural link |
| `updates` | 0.8 | Strong — explicit version chain |
| `relates` | 0.5 | Half — general, unspecified |
| `involves` | 0.3 | Weak — auto-linked; prevents super-hub dominance |
| `typed_relation` | `confidence × 0.9` | Sidecar-supplied confidence ∈ [0,1] |

`hop_decay = 0.5` per hop. At hop 2, even a `derives` edge contributes
`parent_score × 1.0 × 0.25 = 0.25 × parent_score`.

### Configurable cap

The traversal cap is promoted from a hardcoded constant to a config field:

```toml
[search.spreading]
cap = 3                  # max neighbours added (default 3, was hardcoded)
hop_decay = 0.5          # per-hop decay multiplier (default 0.5)

# operator overrides for edge weights (all optional — defaults as above)
[search.spreading.edge_weights]
derives = 1.0
extends = 0.8
updates = 0.8
relates = 0.5
involves = 0.3
typed_relation_max = 0.9  # multiplied by sidecar confidence
```

All fields are optional — the table may be omitted entirely to use defaults.

### Implementation scope

- ~50 LOC change in `src/db/search.rs`.
- No schema change. No new SurrealDB index. No background job.
- Edge type is already present on each graph record — no additional DB reads
  required beyond the existing neighbour query.
- `typed_relation` confidence is already stored on the edge record
  (Migration 012, ADR-013) — no additional fields needed.

### Visibility invariant

Spreading activation already enforces `Visibility` on neighbour candidates.
This ADR does not change that invariant. `Visibility::Sensitive` memories are
excluded from graph traversal regardless of edge weight.

## Consequences

### Positive

- `involves` super-hubs (high-frequency entities) no longer dominate spreading
  activation output. Weight 0.3 means an `involves` neighbour scores at most
  30% of its parent before hop decay — insufficient to surface unless the parent
  score is very high.
- `derives` edges (causal provenance, sidecar-extracted) propagate at full
  strength, rewarding the sidecar investment from ADR-013.
- `typed_relation` edges gain confidence-proportional propagation — high-confidence
  sidecar relations behave like `derives`; low-confidence ones approach `relates`.
- Cap and hop decay configurable — operator tuning without code changes.
- Calibrated against ADR-021 graded eval set for objective measurement.

### Negative — must accept

- Weight table is a new configuration surface. Default values are starting
  heuristics derived from semantic reasoning, not empirically calibrated
  coefficients. Calibration requires the ADR-021 eval set.
- `[search.spreading.edge_weights]` config table adds documentation burden.
  Operators who override one weight implicitly accept responsibility for the
  full table's coherence.
- `typed_relation_max × confidence` introduces floating-point arithmetic on each
  edge traversal. Negligible per-query overhead at graph sizes under 1M edges.

### Trade-offs accepted

- Named weight table over a learned model. A graph neural network or
  Personalized PageRank with heterogeneous edge embeddings would be more
  expressive, but the complexity vastly exceeds the benefit at 10k–100k memory
  scale (see Alternatives).
- `hop_decay = 0.5` is a starting value. At personal-memory scale, hop depth
  rarely exceeds 2 in practice; the exact decay exponent has minor impact
  compared to the edge weight differentiation.

## Alternatives considered

- **Personalized PageRank** — rejected. Evaluated by both researcher and
  devils-advocate review. Three compounding problems: (1) heterogeneous edges
  violate the uniform random-walk assumption, making PPR scores semantically
  opaque; (2) super-hub problem — high-degree entity nodes ("Claude", frequent
  project) accumulate PageRank regardless of semantic relevance; (3) at 100k
  memories with 16M+ edges, per-query PPR requires either offline precomputation
  (invalidated by every write) or online BFS with ~16M edge lookups per query.
  All three are deal-breakers.
- **GraphRAG community detection (Leiden/Louvain)** — rejected. Community
  detection at 10k–100k memory scale with dynamic writes is engineering overhead
  disproportionate to the quality lift. Leiden requires offline batch processing;
  incremental variants are research-grade. Overkill for the stated problem.
- **Deeper BFS with no edge weighting** — rejected. Current state's failure mode.
  Deeper traversal without weighting amplifies the super-hub problem rather than
  mitigating it.
- **Drop `involves` edges from spreading activation entirely** — considered.
  Simpler than weighting, but loses real signal when `involves` edges connect
  genuinely related memories through a non-super-hub entity. Weight 0.3 preserves
  the signal while suppressing dominance.

## Related

- ADR-005: Hybrid retrieval (spreading activation is stage 5 in the pipeline
  defined there; this ADR replaces only the edge-traversal scoring within it).
- ADR-013: Claude SDK sidecar (source of `derives` and `typed_relation` edges
  that benefit most from full-weight propagation).
- ADR-021: Graded-relevance oracle (calibration target for edge weights and cap).
- ADR-022: Adaptive reranker policy (stage 3.5; weighted spreading activation is
  stage 5; both contribute to the Phase A retrieval upgrade).
