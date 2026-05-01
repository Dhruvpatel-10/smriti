# ADR-015: Swap Embedder to GTE-Large-EN-V1.5-Q (int8 quantized)

**Status:** Accepted — Implemented in Phase 0.5 (between P2-B and R8 bench)  
**Date:** 2026-04-30

## Context

`bge-large-en-v1.5` was chosen for the bench harness (ADR-012) for its 1024-dim quality.
In practice it consumes ~1.3 GB RAM just for the ONNX model weights, which causes OOM during
R8 bench runs on a 15 GB box when SurrealDB + the write workload are running concurrently.

Meanwhile `gte-large-en-v1.5-q` (int8 quantized variant, already present in fastembed's
default model enum as `GTELargeENV15Q`) outputs **identical 1024 dimensions**, scores
+1.2 MTEB points higher than bge-large, and weighs ~170 MB — an 8x RAM reduction.

## Decision

Replace `EmbeddingModel::BGELargeENV15` with `EmbeddingModel::GTELargeENV15Q` as the
default model in `bench/src/locomo.rs`. The `LocalEmbedder` match arm for
`"bge-large-en-v1.5"` is retained for backward compatibility with existing `config.toml`
values; users who had that string set keep a working (though no-longer-recommended) path.

The config comment in `src/config.rs` now surfaces `gte-large-en-v1.5-q` as the
recommended 1024-dim option.

No schema migration is required: both models produce 1024-dim float32 vectors; the HNSW
index definition is unchanged.

## Alternatives Considered

| Alternative | Disposition |
|-------------|-------------|
| Stay on `bge-large-en-v1.5` (status quo) | Rejected — blocks R8 bench on 15 GB box due to OOM |
| `Qwen3-Embedding-0.6B` | Deferred to Phase 5 — requires `qwen3` fastembed feature flag; CPU latency benchmark not yet run |
| `Stella-1.5B` | Rejected — ~6 GB RAM, exceeds 100 ms p95 CPU budget on 15 GB box |
| Cloud embedder (Voyage-4-large) | Deferred to Phase 5+ as optional opt-in backend; free tier has TOS privacy gap requiring a payment method on file |

## Consequences

### Existing databases — ACTION REQUIRED

**Vectors stored with `bge-large-en-v1.5` live in a different embedding space from
`gte-large-en-v1.5-q`. After upgrading, vector recall will return garbage until all
memories are re-embedded.**

Operators must run re-embedding once after upgrading:

```
# Via MCP tool (recommended — works regardless of transport):
kb_reembed

# If you have a CLI hook wired up:
smriti --reembed
```

`kb_reembed` re-embeds every active memory in batches and updates the stored vector in
place. The BM25 path remains fully functional during re-embedding; only vector recall
degrades until completion.

### New installs

No action required — fresh databases have no pre-existing vectors.

### Resource impact

| | Before | After |
|-|--------|-------|
| Model RAM | ~1.3 GB | ~170 MB |
| MTEB score | baseline | +1.2 pts |
| Vector dims | 1024 | 1024 (unchanged) |
| Schema migration | — | none required |

Bench R8 is now viable on 15 GB boxes. Production server RAM also drops, freeing
headroom for sidecar processes.
