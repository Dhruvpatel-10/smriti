# ADR-017: Voyage rerank-2.5-lite Cross-Encoder Reranker

**Status:** Accepted — Phase 2
**Date:** 2026-04-30
**Extends:** ADR-016 (Voyage AI cloud embedder backend)

> **Default flipped to `enabled = true` 2026-05-07; see ADR-022.**

## Context

ADR-016 added Voyage AI `voyage-4-large` embeddings, yielding ~+8 MTEB lift over the
local fastembed baseline. Cross-encoder reranking is the standard next step in a
two-stage retrieval pipeline: bi-encoder embeddings score query and document
independently, while a cross-encoder scores them jointly with full attention,
capturing finer-grained relevance signals. Voyage benchmarks show ~+3-5 NDCG@10 lift
on top of `voyage-4-large` retrieval when using `rerank-2.5-lite`.

The user explicitly requested: "use its embedding and reranker for best quality."

## Decision

Add an opt-in second-stage reranker stage at position 3.5 in the 5-stage retrieval
pipeline. Configuration:

```toml
[embeddings.voyage.reranker]
enabled = true
model = "rerank-2.5-lite"
top_k_input = 50    # full RRF pool — no narrowing before rerank
```

### Architecture

- `Reranker` is an enum (mirroring the `Embedder` enum from ADR-016 — same blast-radius
  argument: one current backend, no concrete callsite gain from `dyn Trait` at this
  scale).
  - `Voyage(Arc<VoyageReranker>)` is the only variant today.
- Threaded through `search::recall` and `search::recall_at` as `Option<&Reranker>`.
- Held on `KbServer` as `Option<Arc<Reranker>>` alongside `embedding_client`.
- Inserted at stage 3.5 — between RRF fusion and the entity-anchored person filter.

### Stage 3.5 placement

Three candidate insertion points were evaluated:

| Position | Outcome |
|---|---|
| **After RRF, before entity-filter** ✅ | Reranker reorders the 50-candidate pool while it is maximally diverse. Entity-filter and spreading activation downstream operate on the better-ranked pool. |
| After entity-filter | Entity-filter has already pruned candidates the cross-encoder might have rescued. Loses information. |
| After spreading activation | Spreading activation pulls in graph neighbours — those are unranked and would dominate the rerank input with low-relevance noise. |

### Full-pool input (top_k_input = 50)

The reranker receives the entire RRF pool, not a narrowed top-N. Two reasons:

1. Quality lift compounds — narrowing to top-10 before rerank forecloses candidates
   ranked 11-50 by RRF that the cross-encoder (benchmarks show) often dramatically
   reorders.
2. Cost is bounded — 50 docs × 1 query per recall × ~$0.02/1M tokens ≈ $0.00005/recall.
   The "narrow first to save cost" optimisation is premature at personal scale.

### Document text source

The reranker receives `MemorySummary.summary` (short summary field), not full content:

1. Summary is bounded at 10-500 chars (enforced at write time, per CLAUDE.md), so
   token cost per recall call is predictable.
2. Cross-encoders work well on summary-level text; full content adds noise.
3. Privacy — no content fields cross the wire beyond what already crosses for
   embedding (the Voyage embed path uses summary in the contextual prefix too).

### Soft-fail contract (ADR-005)

On Voyage 5xx after retries, the reranker returns the RRF input order unmodified and
logs `tracing::warn!`. This mirrors the soft-fail contract on the embedding recall path
(ADR-016) — recall must remain best-effort during cloud outages and not turn the daemon
into a brick on a flight.

## Alternatives Considered

| Alternative | Disposition |
|---|---|
| `dyn Reranker` trait | Rejected — same rationale as ADR-016's `dyn Embedder` rejection. One backend today; dynamic dispatch overhead on a per-recall hot path for no callsite gain. |
| Cohere `rerank-3` | Deferred — Voyage already on the wire from ADR-016; fewer auth surfaces, comparable quality. Revisit if Voyage degrades. |
| Local `bge-reranker-large` | Deferred — adds a ~2 GB model to startup, locks the bench harness to a different code path. User explicitly accepted the network-dependency tradeoff for quality. |
| Hard-fail on reranker outage | Rejected — violates ADR-005 best-effort recall contract. |
| Rerank narrowed top-10 only | Rejected — loses the cross-encoder's main quality lever; see "Full-pool input" above. |
| Stage 4.5 placement (after entity-filter) | Rejected — see stage placement table above. |

## Consequences

### Positive

- ~+3-5 NDCG@10 lift on top of Voyage embeddings (Voyage published numbers; measured
  against synth bench in this iteration).
- No changes to BM25, vector, RRF, entity-filter, or spreading activation — bolt-on
  stage.
- Soft-fail keeps recall functional during cloud outages.

### Negative — must accept

- One additional cloud round-trip per recall when enabled (~50-150 ms on home wifi).
  Recall p50 currently 374 ms (synth bench r6); expect p50 ≈ 450-500 ms with reranker.
- Cost: ~$0.00005/recall at personal scale (50 docs × ~50 token summary ×
  $0.02/1M tokens). Free-tier Voyage usage covers personal use indefinitely.
- Reranker adds a second Voyage failure mode — soft-fail mitigates but does not
  eliminate it.

### Trade-offs accepted

- Recall latency regression (~75-125 ms p50) in exchange for cross-encoder quality lift.
- Keeping `Reranker` as an enum (not a trait) bounds future extensibility but avoids
  the blast-radius cost ADR-016 documented for `dyn Embedder`.

### Test coverage

- `voyage_reranker_reorders_by_relevance` — assert reorder by mock relevance scores.
- `voyage_reranker_soft_fails_on_5xx` — assert input order returned on repeated 500.
- `voyage_reranker_empty_input_no_api_call` — assert zero requests sent on empty
  candidate list.
- `voyage_reranker_truncates_to_top_k_input` — assert request size capped at
  `top_k_input`.
- Integration: `bench/src/synth_bench.rs` measures retrieval quality end-to-end.

## Migration notes

For existing operators on Voyage embeddings (ADR-016):

1. The reranker is now enabled by default (`enabled = true`). No `config.toml`
   changes are required for new deployments. For existing deployments, confirm
   `VOYAGE_API_KEY` is set and `embeddings.provider = "voyage"` — the reranker
   is silently skipped if either guard is missing (see `src/server.rs` triple
   guard). Optionally add:
   ```toml
   [embeddings.voyage.reranker]
   enabled = true
   model = "rerank-2.5-lite"
   top_k_input = 50
   ```
2. Restart the server. No DB migration required — reranker is recall-side only.
3. Watch p50/p95 recall latency for the additional cloud-round-trip overhead.
4. To disable: set `enabled = false` and restart.

## Related

- ADR-016: Voyage AI cloud embedder backend (this ADR extends it).
- ADR-018: Voyage Batch API for bench ingest (also extends ADR-016; orthogonal path).
- ADR-005: Hybrid retrieval / best-effort vector contract.
