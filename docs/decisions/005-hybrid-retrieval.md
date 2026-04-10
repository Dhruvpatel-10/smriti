# ADR-005: Hybrid Retrieval with RRF, Spreading Activation, and Lost-in-Middle Ordering

**Status:** Accepted  
**Date:** 2026-04-10  
**Context:** Single-signal retrieval (BM25 alone or vector alone) misses relevant results. BM25 excels at keyword match but misses semantic similarity. Vector search captures meaning but loses exact term matches. Research (Zep, MemGPT, Microsoft GraphRAG) consistently shows hybrid outperforms either alone.

## Decision

Implement a 4-stage retrieval pipeline:

1. **BM25 + Vector parallel query** → two ranked lists
2. **Reciprocal Rank Fusion (RRF)** → single merged ranking
3. **Spreading Activation** → expand via graph edges (1 hop)
4. **Lost-in-the-Middle reordering** → optimize for LLM attention

## Stage 1: Parallel Retrieval

- **BM25:** SurrealDB's built-in `FULLTEXT ANALYZER ... BM25` with `@@` operator. Handles keyword/phrase match.
- **Vector:** Cosine similarity via `vector::similarity::cosine()`. Handles semantic meaning.
- Both queries run with the same filters (type, project, domain, time range).
- Vector query is best-effort: if embeddings are disabled or Ollama is down, falls back to BM25-only.

## Stage 2: Reciprocal Rank Fusion

Formula: `score(d) = sum over sources of 1/(k + rank(d) + 1)` where `k=60`.

- k=60 is the standard from Cormack et al. (2009).
- Items appearing in both BM25 and vector results get summed scores.
- Naturally handles different score scales (BM25 scores vs. cosine similarity).
- Better than simple interleaving: documents that rank high in BOTH signals get boosted.

## Stage 3: Spreading Activation (1-hop graph expansion)

After RRF produces the primary result set:
- Collect memory IDs from top results.
- Traverse `extends`, `derives`, `relates` edges (both directions) to find neighbors.
- Add active neighbor memories not already in results (budget: 20% of limit, min 2).
- This surfaces contextually related memories that might not match the query text but are semantically connected.

Inspired by: Collins & Loftus (1975) spreading activation theory, Microsoft GraphRAG (2024).

## Stage 4: Lost-in-the-Middle Reordering

LLM attention follows a U-curve: strong at the start and end of context, weak in the middle (Liu et al., 2023).

Reorder results: `[rank1, rank3, rank4, ..., rankN, rank2]`
- Best result at position 0 (start — high attention)
- Second-best at position N (end — high attention)
- Lower-ranked items in the middle (low attention — acceptable loss)

## Consequences

- Retrieval quality significantly higher than BM25 alone.
- Vector search adds ~10-50ms latency per recall (embedding the query).
- Graph expansion adds one extra DB query per recall.
- Lost-in-Middle is a zero-cost reorder (in-memory).
