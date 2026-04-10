# ADR-004: EmbeddingGemma-300M via fastembed for Vector Search

**Status:** Accepted  
**Date:** 2026-04-10  
**Context:** Need vector embeddings for semantic similarity search (hybrid with BM25). Requirements: run locally, no external service, best quality/size ratio, on-demand loading, Rust-native.

## Decision

Use `google/embeddinggemma-300m` via the `fastembed` crate (ONNX Runtime), MRL-truncated to 256 dimensions.

## Model Selection Research

| Model | Params | MTEB Score | MRL | Why not |
|-------|--------|------------|-----|---------|
| **EmbeddingGemma-300M** | 308M | SOTA <500M | Yes (768/512/256/128) | **Selected** |
| Jina v5-text-small | 677M | Top <1B, 32k ctx | Yes | 2x larger, marginal gain for short texts |
| nomic-embed-text-v1.5 | 137M | Very good | Yes (768/256) | Lower quality than Gemma |
| all-MiniLM-L6-v2 | 22M | Good | No | No MRL, lower quality |
| Jina v5-text-nano | 239M | Good <500M | Yes | Less tested than Gemma |

## Why 256 Dimensions (MRL Truncation)

Matryoshka Representation Learning (MRL) trains models so that the first N dimensions of the embedding are a valid lower-dimensional embedding. For EmbeddingGemma-300M:

- **768 dims:** Full quality (baseline)
- **256 dims:** ~97% of full quality, **3x less storage**
- **128 dims:** ~94% quality, 6x less storage

For a personal KB (<100k records), 256 dims is the sweet spot: quality is near-full, storage per vector drops from 3KB to 1KB, and brute-force cosine scan stays fast.

## Why fastembed (Not Ollama, Not embed_anything)

| Option | Verdict |
|--------|---------|
| **Ollama HTTP** | Rejected: requires running a separate service. Adds operational complexity for a single-user system. |
| **embed_anything** | Rejected: compile bug (incomplete candle-core pattern match in v0.6.7). Will revisit when fixed. |
| **fastembed** | **Selected:** ONNX Runtime in-process, 44 pre-built models including EmbeddingGemma-300M, lazy loading, `~/.cache/fastembed/` caching, battle-tested. |
| **candle direct** | Considered: maximum flexibility but requires writing tokenization + inference pipeline. Unnecessary since fastembed wraps it cleanly. |

## Implementation

- `src/db/embeddings.rs`: `LocalEmbedder` struct with lazy `OnceLock<Mutex<TextEmbedding>>`.
- Model downloads on first `embed()` call (~200MB ONNX file, cached permanently).
- `spawn_blocking` wraps the synchronous ONNX inference (~10-50ms/text on CPU).
- `spawn_embed_and_store()` runs fire-and-forget on write path (doesn't block `kb_remember`).
- Query embedding in `kb_recall` is inline (must complete before search).

## Config

```toml
[embeddings]
enabled = false                # flip to true when ready
model = "embeddinggemma-300m"  # or "nomic", "bge-small", "minilm", etc.
dimensions = 256               # MRL truncation (0 = full native dims)
```

## Consequences

- First `kb_remember` with embeddings enabled triggers ~200MB download (one-time).
- Each memory stores a 256-float vector (~1KB) alongside its text.
- Hybrid retrieval: BM25 + cosine similarity, fused via RRF (ADR-005).
- Binary size increases ~5MB (ONNX Runtime linkage).
