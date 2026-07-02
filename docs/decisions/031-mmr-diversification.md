# ADR-031: MMR Post-Fusion Diversification

**Date:** 2026-06-06
**Status:** Accepted
**Extends:** ADR-022 (adaptive reranker policy), ADR-023 (weighted spreading activation), ADR-021 (graded-relevance oracle)

## Context

The 2026-05-07 LoCoMo10 bench (STATUS dashboard) showed the bottleneck moving from
ranking to retrieval. After the reranker default-on flip, Recall@1 jumped 4.7× (5 → 24)
but Hit@10 stayed flat at 50% — the reranker pulls existing rank 2-4 hits up, it does not
add new hits. Answer accuracy stalled at ~63%, gated by a 44% retrieval-miss rate.

The diagnosed failure mode: on aggregation and list queries ("what did I decide about X",
"all my action items on Y"), one cluster of near-identical memories floods the top-K fusion
pool. Five paraphrases of the same decision occupy five of the ten slots, squeezing out
other relevant-but-distinct clusters before the reranker or spreading-activation stages
ever see them. The reranker cannot recover a memory that fusion already truncated.

STATUS listed three candidate levers for the next bench cycle: MMR diversity (free),
HyDE (~$0.001/query), and larger candidate K. MMR is the cheapest — no extra API call,
pure local vector math over candidates already fetched — and directly targets the
cluster-flooding failure mode.

## Decision

Add a Maximal Marginal Relevance diversification stage **between Fusion (step 3) and
Adaptive Rerank (step 3.5)**, numbered **step 3.4** in the pipeline. It reorders the fused
candidate pool to penalize near-duplicates before the reranker sees it, so a diverse set of
clusters survives into the top-K.

### Algorithm

Greedy MMR selection:

```
score(c) = lambda · relevance(c) − (1 − lambda) · max_cos_sim(c, already_picked)
```

- `relevance(c)` is the fusion score, min-max normalized to [0, 1] per query.
- `max_cos_sim(c, picked)` is the highest cosine similarity between candidate `c` and any
  already-selected memory, computed over stored embedding vectors.
- First pick is the highest-relevance candidate (no diversity term — nothing picked yet).
- Each subsequent pick maximizes the combined score, balancing relevance against redundancy.

`lambda` defaults to **0.7** — relevance-leaning. `lambda = 1.0` collapses to pure
relevance (no diversity); `lambda = 0.0` is pure diversity (ignores fusion rank).

### Config (`[search.mmr]`)

```toml
[search.mmr]
enabled = true   # default-on
lambda  = 0.7    # relevance vs diversity weight, validated to [0.0, 1.0] at startup
```

`AppConfig::validate` rejects `lambda` outside `[0.0, 1.0]` with a `KbError::Config`.

### Skip conditions

The stage is a no-op (fusion order passes through unchanged) when any of:

- `mmr.enabled = false`
- BM25-only path — no vector candidates exist (`vector_memories.is_empty()`)
- the query carried no embedding (`query_vector` was `None`)
- ≤1 candidate in the pool
- candidate vectors could not be fetched (soft-fail → empty map → no-op)

### Vector fetch

`fetch_candidate_vectors` pulls stored embeddings for the fused candidate IDs in a single
`SELECT record::id(id) AS rid, vector FROM memory WHERE record::id(id) IN $ids` round-trip.
IDs missing a `vector` field are silently omitted. Candidates without a vector participate
in relevance-only order and are appended after the MMR-selected set, preserving their
relative fusion rank — they are never dropped.

### Wiring

- `MmrConfig` threaded through `KbServer` → `recall::execute` / `recall_at::execute` →
  `search::recall` / `search::recall_at`.
- Both `recall` and `recall_at` run the stage identically (recall_at uses the same config;
  no caller override, mirroring the reranker's recall_at treatment per ADR-022).
- New `RecallStage::Mmr` histogram in `recall_metrics`; surfaced in `kb_status`
  `recall_stage_p95` as the `mmr` field. `count = 0` when disabled or skipped.

## Consequences

### Positive

- Targets the diagnosed cluster-flooding failure mode on aggregation/list queries — the
  class that the reranker provably cannot fix (Hit@10-bound).
- Zero marginal API cost. Pure local cosine math over vectors already in the DB; one extra
  batched `SELECT` per recall (bounded by candidate pool, ≤50).
- Fully config-driven and reversible: `enabled = false` restores exact pre-ADR-031 order,
  verified by the `mmr_skips_when_disabled` test asserting byte-identical ordering.
- New stage histogram keeps the pipeline observable — `mmr` p95 visible in `kb_status`.

### Negative

- Greedy MMR is O(n²·k) in cosine comparisons (n = pool size, k = picked). Bounded by the
  ≤50 candidate cap; negligible against the ~310ms Voyage rerank RTT it precedes.
- One additional batched DB round-trip per recall to fetch candidate vectors. Soft-fails to
  a no-op on error rather than failing the recall.
- Adds a tunable (`lambda`) whose optimum is corpus-dependent. 0.7 is a relevance-leaning
  default, not yet bench-calibrated (see below).

### Trade-offs accepted

- **Default-on, not yet bench-measured.** Shipped `enabled = true` on the reasoning that
  the failure mode is well-diagnosed and the skip-conditions make it inert on the queries
  it can't help (BM25-only, single-cluster, no-embedding). The quality delta is pending the
  next LoCoMo10 cycle; if the bench shows regression, the flip to `enabled = false` is a
  one-line config change with no code revert. STATUS open-levers updated accordingly.
- **lambda = 0.7 over a learned weight.** A per-query-type adaptive lambda (mirroring the
  ADR-022 reranker policy) is the natural follow-up but is deferred until the bench
  confirms a flat lambda helps at all.

## Alternatives considered

- **Larger candidate K (30 → rerank to 10)** — also on the STATUS lever list. Complementary,
  not exclusive: a bigger pool gives MMR more clusters to diversify across. Deferred to a
  separate change because it raises rerank API cost (more candidates sent to Voyage),
  whereas MMR is free. Can stack on top of MMR later.
- **HyDE for entity/topic queries** — addresses the *miss* rate (memories never retrieved),
  a different axis than MMR (memories retrieved but crowded out). Higher swing on profile/
  list queries but costs ~$0.001/query. Tracked separately; not mutually exclusive.
- **Dedup by content hash before fusion** — rejected. Near-duplicates are semantically
  similar, not byte-identical; a hash misses paraphrases, which is exactly the case MMR
  handles. Cosine over embeddings is the correct similarity measure here.
- **MMR after rerank instead of before** — rejected. Diversifying after the reranker has
  already truncated to top-K cannot recover a cluster the reranker dropped. The point is to
  feed the reranker a diverse pool, so MMR must precede it (step 3.4 < 3.5).

## References

- [ADR-021](021-graded-relevance-oracle.md) — bench oracle that diagnosed the Hit@10 ceiling
- [ADR-022](022-adaptive-reranker-policy.md) — reranker default-on; MMR feeds its candidate pool, and the per-call-override / recall_at-no-override pattern is mirrored here
- [ADR-023](023-weighted-spreading-activation.md) — spreading activation runs after rerank; a more diverse pre-rerank pool seeds more distinct entity anchors
- STATUS dashboard (`docs/agents-docs/STATUS.md`) — "Open levers (next bench cycle)" listed MMR as the free, highest-priority lever

**Code references:**

- `src/db/search.rs` — `cosine_sim`, `fetch_candidate_vectors`, `mmr_diversify`; step 3.4 wiring in `recall` and `recall_at`
- `src/config.rs` — `MmrConfig`, `[search.mmr]`, `lambda` range validation in `AppConfig::validate`
- `src/db/recall_metrics.rs` — `RecallStage::Mmr` histogram and `RecallStagePercentiles.mmr` wire field
- `tests/search_retrieval.rs` — `mmr_skips_when_disabled`, `mmr_lambda_one_equals_relevance_only`, `mmr_diversifies_clustered_pool`
