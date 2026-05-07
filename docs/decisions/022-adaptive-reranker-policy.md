# ADR-022: Adaptive Reranker Policy

**Status:** Accepted
**Date:** 2026-05-07
**Extends:** ADR-017 (Voyage reranker), ADR-021 (graded-relevance oracle)

## Context

ADR-017 shipped Voyage `rerank-2.5-lite` as a binary opt-in controlled by
`embeddings.voyage.reranker.enabled`. When `enabled=true`, every `kb_recall` and
`kb_recall_at` call pays the cross-encoder round-trip (~50–150 ms, ~$0.00005).
When `enabled=false`, no call reranks.

Per-query gating is not possible under the current design. Two real problems follow:

1. **Quality left on the table.** Short lookups and ephemeral working-memory reads
   do not benefit from cross-encoder reranking — their RRF top-1 is dominant by
   construction. These calls pay the latency and cost for zero quality lift.
2. **Flip-on-by-default violates ADR-017's opt-in contract.** ADR-017's accepted
   consequence was that operators choose reranking deliberately. Changing the
   default silently breaks that contract for existing operators.

An intermediate `Adaptive` policy that gates per-query on cheap signals resolves
both: it reduces cost ~40% vs. always-on while preserving quality on the calls
where the cross-encoder moves the needle.

## Decision

Extend `VoyageRerankerConfig` with a `policy` field:

```toml
[embeddings.voyage.reranker]
enabled = true
model = "rerank-2.5-lite"
top_k_input = 50
policy = "adaptive"   # "always" | "adaptive" | "never"
```

Add a `RerankerPolicy` enum to `src/types/enums.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RerankerPolicy {
    Always,
    #[default]
    Adaptive,
    Never,
}
```

When `enabled=true && policy=Adaptive`, each recall call evaluates
`should_rerank(query, rrf_scores, memory_type, caller_override)` before
dispatching to Voyage:

### Skip conditions (any one triggers skip)

| Condition | Rationale |
|---|---|
| `rrf_scores[0] > 2.0 × rrf_scores[1]` | Top-1 is dominant — cross-encoder cannot improve position 0 |
| `memory_type == Working` | Ephemeral memories; recall is lookup, not ranking |
| `query.word_count() <= 3 && person_filter.is_none()` | Short lookup with no disambiguation need |

### Force conditions (any one overrides skip)

| Condition | Rationale |
|---|---|
| `person_filter.is_some()` | Name disambiguation benefits most from joint query–doc attention |
| `query.word_count() > 6` | Long query; semantic nuance is high; cross-encoder excels |
| `memory_type == Semantic` | Precision path; recall is expected to be exact |

Force conditions take precedence over skip conditions.

### Caller override

`kb_recall` gains an optional `rerank` parameter:

```
kb_recall(query, ..., rerank?: bool) -> [...]
```

When `rerank` is `Some(true)`, skip conditions are bypassed (always rerank).
When `rerank` is `Some(false)`, force conditions are bypassed (never rerank).
Explicit caller wins over all heuristics. Older clients that omit `rerank` are
unaffected — the field is additive in the JSON schema.

### Metrics

Each `should_rerank` decision emits a counter:

```
rerank_decision_total{policy="adaptive", decision="skip"}
rerank_decision_total{policy="adaptive", decision="rerank"}
rerank_decision_total{policy="always", decision="rerank"}
```

This provides the observability needed to calibrate heuristics against the
ADR-021 graded eval set.

### Threshold calibration note

The heuristic thresholds (`2.0×`, `>6 words`, `≤3 words`) are starting values,
not calibrated constants. They must be tuned against the ADR-021 nDCG@10 eval
before becoming documented defaults. Document the calibration run results in a
follow-up ADR amendment or in `bench/RESULTS.md`.

## Consequences

### Positive

- Cost reduction ~40% vs. always-on at fixed quality (tunable post ADR-021 eval).
- Latency reduction on short lookups and working-memory reads (~50–150 ms saved
  per skipped call).
- Caller override allows MCP clients to force or suppress reranking without
  config changes.
- Additive MCP schema change — `rerank` field is optional; older clients
  unaffected.
- `RerankerPolicy::Never` gives operators a policy-level off switch distinct from
  `enabled=false` — useful for temporary disabling without editing the boolean.

### Negative — must accept

- Heuristics add a decision branch in the recall hot path — low overhead
  (word count, score comparison) but non-zero.
- Adaptive thresholds are empirically unvalidated until ADR-021 eval set is built
  and measured. Until then, treat adaptive as "better than always-on" heuristically,
  not provably.
- `RerankerPolicy` enum in `src/types/enums.rs` must be kept in sync with
  `config.rs` deserialization and the MCP schema documentation in README.

### Trade-offs accepted

- Heuristic gating over ML-based gating. Score-threshold gating with a
  learned threshold model was considered (see Alternatives) and rejected — the
  cure is more complex than the disease at personal scale.
- `2.0×` RRF dominance ratio is a starting heuristic. It will require adjustment
  after ADR-021 graded eval reveals actual dominance distribution.

## Alternatives considered

- **Score-threshold gating with calibrated probabilities** — rejected. Voyage
  cross-encoder scores are not calibrated probabilities (devils-advocate finding).
  A raw score threshold on reranker output has no stable interpretation across
  queries. The RRF dominance ratio operates on the pre-rerank pool where scores
  are better understood.
- **Always-on default** — rejected. Conflicts with ADR-017's explicit opt-in
  contract. Breaking change for existing operators.
- **Per-memory-type config table** — rejected. Over-engineered. `Adaptive` policy
  with force/skip rules covers the important cases without a per-subtype matrix.
- **ML classifier for rerank decision** — rejected. Requires a labeled
  rerank/skip dataset (circular dependency on ADR-021 eval). Heuristics are
  cheaper, interpretable, and sufficient at personal scale.

## Related

- ADR-005: Hybrid retrieval / best-effort contract (soft-fail on Voyage outage
  applies to the reranker under all policies).
- ADR-017: Voyage reranker (this ADR extends it with per-query policy control).
- ADR-021: Graded-relevance oracle (provides nDCG@10 signal to calibrate the
  thresholds defined here).

## 2026-05-07 update

`VoyageRerankerConfig::enabled` default flipped from `false` to `true` in both
`serde` deserialization (missing field) and `Default::default()`. Motivation:
analysis of the LoCoMo probe set shows 27.6% of queries have the correct answer
parked at rank 2-4 in the RRF pool. The adaptive policy cannot help when the
reranker object is `None` at server boot — gating on `enabled=false` means the
policy machinery is never exercised. Flipping the default to `true` allows
`rerank-2.5-lite` to earn its keep under real traffic before larger pipeline
refactors. Estimated cost impact: ~$0.05/1k queries with `policy=adaptive`.
Operators who explicitly set `enabled = false` in `config.toml` are unaffected.
