# ADR-020: Embedder trait, voyage-context-3, and chunking architecture overhaul

**Date:** 2026-04-30
**Status:** Accepted

## Context

The current chunker (`src/db/chunking.rs:24-29`) and embedder layer have several
compounding defects:

### Embedder layer
- `Embedder` is an **enum** (`Local(LocalEmbedder)` | `Voyage(VoyageEmbedder)`),
  not a trait. Adding a new backend (OpenAI, Cohere, hosted Jina, etc.) requires
  changing every dispatch site. Per-call branching leaks provider knowledge into
  business logic (`if matches!(embedder, Embedder::Voyage(_))` patterns).
- Provider-specific knobs (chunk size, max input tokens, asymmetric `input_type`
  support, contextualized-chunk-batch support) are hardcoded as constants in
  callers rather than queried from the backend.
- Voyage client targets `/v1/embeddings` only. No path for `voyage-context-3`'s
  `/v1/contextualizedembeddings` endpoint.

### Chunker
- `CHUNK_SIZE = 1500` chars (~350 tokens) — undersized. Production median is
  800 tokens (OpenAI 800/400, AnythingLLM 1000/20, Anthropic cookbook 800,
  Khoj 256 with heading prepend).
- `CHUNK_OVERLAP = 200` chars (~50 tokens).
- `CHUNK_THRESHOLD = 2000` chars to trigger.
- Boundary preference: `\n\n` → `. ` → hard cut. No markdown-heading
  awareness. No code-fence preservation.
- **Critical — context misalignment** (`chunking.rs:149-154`):
  `spawn_embed_and_store` receives raw chunk text only. No parent summary, no
  `contextual_text` prefix. Chunk vectors live in a different vector subspace
  than parent vectors — hybrid RRF fusion mixes incommensurable distances.
- **Critical — reranker blind** (`db/reranker/voyage.rs:107-110`): the reranker
  (ADR-017) receives the chunk `summary` field
  (`"[chunk 2/5] truncated parent summary"`, ~80 chars) not chunk content. The
  cross-encoder scores against a useless 80-char proxy.
- **`kb_reembed` strands chunk vectors**: `reembed.rs:46-49` re-embeds `summary`
  for all rows. Chunk rows get the 80-char fragment label embedded silently
  on model switch.
- **Orphan chunks after TTL expiry**: `prune.rs::expire_ttl` archives parents
  but does not cascade to `extends`-children.
- **UTF-8 panic risk**: `text.len()` byte-length used at `chunking.rs:64,82`.
  Slicing at a byte offset that falls inside a multi-byte codepoint panics.

### Voyage opportunity
Voyage AI released `voyage-context-3` (July 2025): a single
`/v1/contextualizedembeddings` API call accepts `inputs: [[chunk1, chunk2, ...]]`
and returns N chunk vectors per doc with full-document context baked in. Voyage's
own benchmark shows +23.66% over late chunking and +6.76% over Anthropic
Contextual Retrieval. Cost: $0.18/M tokens (200M free per account). Native 1024
dimensions (Matryoshka 256/512/1024/2048). 32K tokens per doc.

**Key fact (researcher 2026-04-30):** `voyage-context-3` is exclusively available
via `/v1/contextualizedembeddings`. It supports `input_type=query` (single-element
list) and `input_type=document` (multi-chunk list) on the same endpoint. Vectors
across both modes share the same cosine-similarity space. Sources:
- https://docs.voyageai.com/docs/contextualized-chunk-embeddings
- https://blog.voyageai.com/2025/07/23/voyage-context-3/

This means a unified single-model strategy is correct: voyage-context-3 for
queries, parents, and chunks — one endpoint, one vector space.

## Decision

### 1. Replace `Embedder` enum with `Embedder` trait

```rust
#[async_trait]
pub trait Embedder: Send + Sync + 'static {
    fn fingerprint(&self) -> EmbedderFingerprint;
    fn dimensions(&self) -> usize;
    fn max_input_tokens(&self) -> usize;
    fn max_chunks_per_doc(&self) -> usize { 1 }
    fn recommended_chunk_tokens(&self) -> usize { 350 }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
    async fn embed_document(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed N chunks of the same parent doc. Default = N independent
    /// `embed_document` calls. Backends with contextualized-batch support
    /// (Voyage) override to a single API call.
    async fn embed_document_chunks(
        &self,
        chunks: &[String],
    ) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(chunks.len());
        for c in chunks {
            out.push(self.embed_document(c).await?);
        }
        Ok(out)
    }
}
```

Storage: `Arc<dyn Embedder>` everywhere. Factory at
`src/db/embeddings/factory.rs::build_embedder(cfg)` returns
`Arc<dyn Embedder>` based on config (`local | voyage | <future>`).

Module layout:
```
src/db/embeddings/
├── mod.rs              # re-exports + factory
├── traits.rs           # Embedder trait, EmbedderFingerprint
├── factory.rs          # config → Arc<dyn Embedder>
├── fingerprint.rs      # _kb_metadata gate (existing)
├── local.rs            # LocalEmbedder (fastembed)
└── voyage/
    ├── mod.rs          # VoyageEmbedder
    ├── client.rs       # HTTP client
    ├── contextualized.rs  # /v1/contextualizedembeddings
    └── wire.rs         # request/response types
```

Provider-specific branches in callers (chunking, reranker, reembed) are removed —
the trait answers via `recommended_chunk_tokens()`, `max_chunks_per_doc()`, etc.

### 2. Adopt voyage-context-3 — unified endpoint

VoyageEmbedder targets **`/v1/contextualizedembeddings` exclusively** for all
three trait methods (`embed_query`, `embed_document`, `embed_document_chunks`).
voyage-context-3 is only available on this endpoint; `/v1/embeddings` is not
supported for this model.

- Single-element query calls: `input_type=query`, one-element inputs list.
- Single-element document calls: `input_type=document`, one-element inputs list.
- Multi-chunk document calls: `input_type=document`, N-element inputs list
  (contextualized batch — full-document context baked in per chunk).

All three produce vectors in the same cosine-similarity space (Voyage confirms
cross-call-shape compatibility for the same model and endpoint).

`max_chunks_per_doc()` returns 1000 (Voyage contextualized-endpoint cap).
`recommended_chunk_tokens()` returns 800. `max_input_tokens()` returns 32000.

Embedder fingerprint becomes `voyage/voyage-context-3/1024`. Mismatch with
existing stored vectors refuses startup. Operators receive a clear error
message: "embedder fingerprint mismatch; run `kb_reembed` then restart, or set
`SMRITI_FORCE_REEMBED_ON_START=1` to auto-reembed on next startup." The escape
flag prevents systemd `Restart=always` crash loops on stuck operators.

### 3. Local fastembed fallback uses Khoj-style structural prefix

LocalEmbedder advertises `max_chunks_per_doc() = 1` (no native contextualization).
Default trait fallback for `embed_document_chunks` runs N independent
`embed_document` calls. The chunking pipeline detects this branch via
`embedder.max_chunks_per_doc() > 1` and applies a structural prefix
(`prepend_structural_prefix`) to each chunk before passing them to
`embed_document_chunks` — prepending the nearest preceding markdown heading or
section marker. Deterministic. Free. Approximates voyage-context-3's
full-document contextualization.

**Trait contract** (`#[doc]` on `embed_document_chunks`): when
`max_chunks_per_doc() == 1`, the **caller** is responsible for applying any
desired contextualization (structural prefix, summary prepend, etc.) before
invoking the method. The default impl embeds each chunk independently as-given.
This contract is enforced at the doc-comment level so future callers cannot
silently bypass it.

The trait default impl runs sequentially (`for c in chunks { ... await }`) —
not `try_join_all` — because (a) `&self` cannot be moved across a `tokio::spawn`
boundary, and (b) `LocalEmbedder` serialises through a single
`Mutex<TextEmbedding>` regardless. Documented in code so contributors don't
"optimise" this into a deadlock.

`recommended_chunk_tokens()` returns 350 (fastembed default model max).

### 4. Recursive structural splitter

New `chunking.rs::split_into_chunks` driven by trait values, not constants:

```rust
pub fn split_into_chunks(
    text: &str,
    target_tokens: usize,    // from embedder.recommended_chunk_tokens()
    max_tokens: usize,       // from embedder.max_input_tokens()
    overlap_tokens: usize,   // hardcoded 50
) -> Vec<String>
```

- **Separator hierarchy**: `["\n## ", "\n# ", "\n\n", "\n", ". ", "? ", "! ", " "]`.
  Markdown-heading aware.
- **Code-fence preservation**: never split inside a `` ``` `` block.
- **UTF-8 safe**: `floor_char_boundary` everywhere a byte slice is taken.
- **Token estimation**: `chars / 4` heuristic for sizing (cheap, accurate enough
  for chunking decisions; precise tokenization is the embedder's job).

### 5. Reranker payload fix — internal `RerankCandidate`

Current bug: `db/reranker/voyage.rs:107-110` passes `MemorySummary.summary`
(80-char chunk label) to the cross-encoder.

Fix: introduce internal struct (not part of MCP wire shape):
```rust
pub(crate) struct RerankCandidate {
    pub summary: MemorySummary,
    pub content: Option<String>,  // pulled from row.content
}
```

`db/search.rs` SELECTs `content` alongside other fields when building rerank
candidates, constructs `Vec<RerankCandidate>`, passes that to
`Reranker::rerank`.

**Uniform payload construction** — to avoid biasing the cross-encoder toward
chunks (long content) over parents (short summaries) in mixed result sets, the
reranker builds candidate text via:

```rust
fn build_rerank_text(c: &RerankCandidate) -> String {
    let body = c.content.as_deref().unwrap_or(&c.summary.summary);
    truncate_utf8(body, 3200)  // floor_char_boundary
}
```

Every candidate gets the same construction rule — content if present, summary
otherwise, truncated to 3200 chars. No conditional length disparity. Mixed
chunk+parent result sets score on comparable text lengths.

`MemorySummary` shape (MCP recall response) is **not** changed.

### 6. `kb_reembed` chunk-aware (graph-based detection)

`reembed.rs::run`: SELECT chunk identity from **graph structure**, not summary
string pattern. A user's non-chunk memory whose summary genuinely begins
`[chunk 1/2]` (e.g., journaling about chunking) would otherwise be misidentified
and re-embedded from `content` instead of `summary`, corrupting its vector
permanently.

```surql
SELECT
    record::id(id) AS mid,
    summary,
    content,
    IF array::len(->extends->memory) > 0 THEN true ELSE false END AS is_chunk,
    array::first(->extends->memory.id) AS parent_id
FROM memory WHERE BITEMPORAL_ACTIVE
ORDER BY created_at ASC LIMIT $limit START $offset
```

Edge direction: `RELATE chunk -> extends -> parent`, so chunks have **outbound** extends edges. `->extends->memory` on a chunk row traverses to its parent(s); count > 0 identifies chunk rows. `<-extends<-memory` would identify parents (rows that are extended-into), which is the inverse.

For `is_chunk = true` rows, embed `content`; else embed `summary`. Chunk rows
where `content IS NULL` (shouldn't happen post-creation, but defensive) skip
re-embed and emit a warn-level log.

**Sibling-batch optimisation**: when `embedder.max_chunks_per_doc() > 1`, group
sibling chunks (same parent via `extends`) and re-embed them via
`embed_document_chunks` in one batched call. Restores voyage-context-3
contextualization on re-embed and unlocks ADR-018 Voyage Batch API discounts.
Without this, a 50K-chunk re-embed runs 50K serial single-element calls (~67
minutes at 80ms each).

Model switches no longer strand chunk vectors silently.

### 7. TTL via `expires_at` propagation (no cascade UPDATE)

Devil's advocate identified the original "cascade UPDATE" design as either
redundant or non-atomic across two SurrealQL statements. Resolved by making
the `expires_at` propagation the **sole** mechanism:

- `chunking.rs` already calls `.maybe_expires_at(parent.expires_at)` on the
  chunk's `MemoryDraft` (`chunking.rs:137`). Chunks inherit the parent's TTL.
- `BITEMPORAL_ACTIVE` predicate (`db/mod.rs:32`) already filters on
  `expires_at`. When the parent expires, all chunks with the same `expires_at`
  expire on the same predicate evaluation — no separate UPDATE needed.
- Add an invariant test: `chunk.expires_at == parent.expires_at` at all
  creation paths. Catches regression if chunking ever drops the propagation.
- Add a second invariant test: when parent `expires_at` ticks past, both parent
  and chunks vanish from `recall` results in the same query — no orphan window.

`prune.rs::expire_ttl` is unchanged. The cascade UPDATE is **not** added. The
`status='archived'` index hint stays parent-only; chunks rely purely on
`expires_at` for TTL semantics. This eliminates the transaction-ordering risk
and reduces code surface.

For non-TTL archive paths (`kb_forget`, supersede, retract): chunks DO need
explicit cascade because those paths don't propagate via a shared field. Those
remain unchanged from current behaviour (already cascade-archive via the
`extends` relation in `db/memory.rs::archive` → confirm during implementation;
add cascade if missing).

### 8. Auto-merge at retrieval (deferred)

When ≥2 sibling chunks of the same parent appear in the top-K result set, fold
them into a single parent return and drop the siblings (LlamaIndex
AutoMergingRetriever pattern). Reduces BM25 sibling-flooding and
spreading-activation chunk explosion.

Tracked as ADR-021 follow-up. Not in this round.

## Implementation phases

**Phase 1 — pure refactor, zero behavior change.**
Define trait + module split. Wrap existing `LocalEmbedder` and current
`VoyageEmbedder` (still on `/v1/embeddings`, still on `voyage-4-large`) behind
the trait. Replace `Arc<Embedder>` enum at all call sites with
`Arc<dyn Embedder>`. Tests must stay at 394 pass / 0 clippy warnings.

**Phase 2 — behavior changes.**
Switch `VoyageEmbedder` model to `voyage-context-3`. Route all three embed
methods (`embed_query`, `embed_document`, `embed_document_chunks`) through
`/v1/contextualizedembeddings` exclusively (only endpoint that supports
voyage-context-3). Add Voyage override for `embed_document_chunks` (single
batched call) — and **delete `spawn_embed_and_store`** in the same change so
the chunking pipeline cannot accidentally double-embed via the legacy
fire-and-forget path.
Restructure chunking pipeline to collect-then-batch. Apply structural-prefix
prepend on local path before calling `embed_document_chunks`. Reranker
`RerankCandidate` struct + uniform-payload content threading. `kb_reembed`
graph-based chunk detection + sibling-batch re-embed. UTF-8 safety in splitter.
Token-estimation-based separator hierarchy. Fingerprint update + escape flag
(`SMRITI_FORCE_REEMBED_ON_START`) + migration prompt.

### Phase 2 quality + perf gates (must pass before merge)

- `cargo test`: 394 → ~561+. Zero failures. (2 ignored: voyage integration, key-gated.)
- `cargo clippy --all-targets -- -D warnings`: zero warnings.
- **Latency benchmark**: p99 `embed_query` on `/v1/contextualizedembeddings` with 1-element
  call ≤100ms over 100 samples (Voyage backend). Caught only by integration
  bench against live API, gated behind a `VOYAGE_API_KEY` env var; skipped in
  default CI.
- **Vector-space invariant test**: `cosine_similarity(embed_query("x"),
  embed_document("x"))` ≥ 0.85 on Voyage backend. Confirms single-endpoint
  cross-call-shape same-space claim holds in practice for the chosen model.
- **Double-embed guard test**: insert a long-content memory, assert that
  `vector_dim` field on each chunk row matches voyage-context-3 dimensions
  (1024) and that no race-overwrite produced a wrong-dim vector.

## Consequences

### Positive
- New backends (OpenAI, Cohere, hosted Jina) = implement trait + factory match
  arm. No call-site changes. **Strongly typed** provider abstraction.
- Chunk vectors carry full-document context via voyage-context-3 — directly
  addresses parent-vector-space misalignment.
- Best-published quality numbers in 2025-2026 RAG benchmarks (Voyage's own
  bench).
- Free at personal-memory scale — 200M free tokens covers ~100K typical memories.
- No LLM-per-chunk latency or cost tax (contrast: Anthropic Contextual Retrieval).
- Reranker quality recovers — cross-encoder now sees real content.
- Model switches no longer strand chunk vectors silently.
- Orphan chunks no longer linger after parent archival.
- UTF-8 panic path eliminated.
- Test suite gets a `MockEmbedder` impl — drops fastembed model load from
  hermetic tests, faster CI. **MockEmbedder must be content-aware**: vectors
  derived from a hash of token n-grams (not orthogonal random per text), so
  similar text → similar vector. Naive random-per-string mocks fake-green dedup
  tests because cosine clusters around 0 for any two distinct texts. The
  existing 0.82/0.92 dedup-threshold tests stay valid only with a content-aware
  mock; or those tests pin to `LocalEmbedder` explicitly.

### Negative
- Voyage path locked to voyage-context-3; local fastembed fallback is strictly
  weaker (structural prefix vs. full-document contextualization).
- Embedder fingerprint extension required — operators migrating from
  voyage-4-large to voyage-context-3 must run `kb_reembed`. Cannot be skipped;
  mixed vector spaces corrupt retrieval.
- 32K-token-per-doc limit on voyage-context-3 — extreme-long memories (full
  PDFs > ~96K chars) must be split across multiple context-3 calls, losing
  cross-window context. Rare at personal-memory scale; accepted.
- Two-phase migration; each phase must independently keep tests green.

### Trade-offs accepted
- Voyage path diverges from local path more than before. Local fallback
  produces lower-quality chunk vectors; this was always true — the structural
  prefix makes the gap explicit rather than hiding it.
- Operators who never adopted Voyage (ADR-016) are unaffected by Voyage-specific
  points but still receive the trait refactor, splitter rewrite, reranker fix,
  reembed branch, TTL cascade, and UTF-8 safety regardless of backend.

## Alternatives considered

- **Keep enum, add fourth variant for voyage-context-3** — N variants for N
  backends, dispatch sites grow linearly with backend count. Trait scales O(1).
- **Late chunking (Jina, arXiv 2409.04701)** — Voyage REST returns only pooled
  vectors, not token-level representations. Implementing late chunking requires
  self-hosting an open-weight long-context model, abandoning Voyage's
  asymmetric `input_type` advantage and reranker integration. Dead end on this
  backend.
- **Anthropic Contextual Retrieval (LLM prefix per chunk)** — Privacy-safe on
  paid Anthropic API (Commercial Terms §B). voyage-context-3 outperforms it
  +6.76% per Voyage's bench, costs ~6× less ($0.18/M vs $1.02/M), zero latency
  tax, no second API key. Retained as a documented fallback option behind a
  config flag for environments without a Voyage key.
- **ColBERT / late interaction** — 25× storage overhead; Voyage does not
  support it; storage explosion at personal-memory scale unacceptable.
- **RAPTOR** — paper-only; clustering + summarization complexity high;
  benchmark gain on synthetic QuALITY does not transfer reliably.
- **Semantic chunking (cosine-distance valley)** — no shipping production
  system uses it; AnythingLLM has it as an open issue; adds 10-30× embedding
  calls during ingest.
- **24K-char Voyage chunks ("fit the window")** — voyage-context-3 sees the
  full document regardless of chunk size, making "maximize context window"
  argument moot. Smaller chunks remain preferable for reranker payload size and
  retrieval precision.
- **LLM-as-chunker (Mem0 / Letta pattern)** — solves atomic fact extraction
  from conversational text, not long-content structural chunking. Different
  problem.

## Related

- ADR-016: Voyage AI cloud embedder; paid-tier privacy gate — voyage-context-3
  inherits the same gate and the same fingerprint-at-startup mechanism.
- ADR-017: Voyage reranker — reranker payload fix (§5) is consolidated here;
  ADR-017's document-text-source rationale is superseded for chunk rows.
- ADR-018: Voyage Batch API — `/v1/contextualizedembeddings` is supported by
  the Batch API for 33% discount on bulk re-embed.
- ADR-019: Channel/modality split — the `[channel/modality]` token in
  `contextual_text` flows into the structural prefix prepended on the local
  fallback path.
