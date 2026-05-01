# ADR-014: Write-Time Anchor Entity Extraction (Phase 2-B)

**Status:** Accepted, Implemented  
**Date:** 2026-04-30  

---

## Context

Hybrid retrieval (BM25 + vector) has a structural gap: **promise-tracking and entity-faceted
recall**. When a user stores "Decided to migrate Auth0 to Clerk for the Helix project", BM25
matches on the literal tokens present. A later query like "what did we decide about our auth
vendor?" uses different vocabulary — "vendor" does not appear in the summary, "Clerk" and "Auth0"
do not appear in the query. The vector stage partially compensates, but the BM25 stage misses the
entities entirely, reducing RRF fusion quality.

This is a product issue, not a benchmark artefact. Entity-faceted recall (e.g. "what have I
stored about Clerk?"), promise tracking (resolving action items by person name), and
cross-project entity linking all depend on the same capability: knowing which named entities
anchor a memory at write time.

The problem is in the **summary word ↔ query word mismatch** category. Recall-time query
rewriting (IRCoT-lite, Phase 2-A) addresses this from the query side but was deferred because
it adds latency and LLM cost to every recall call, regardless of need. The write-time approach
costs one sidecar call at write time and zero at recall time.

---

## Decision

At write time, extract up to 3 **anchor entities** from the memory summary using the LLM sidecar,
store them in a new `memory.anchor_entities: option<array<string>>` field (MIGRATION_014), and
index that field with a dedicated BM25 fulltext index (`idx_memory_anchor_ft`). The BM25 query
in `kb_recall` and `kb_recall_at` is extended with an `OR anchor_entities @@ $query` clause.

Anchors are proper nouns, project names, technology names, event names, and key noun phrases
extracted verbatim — no lowercasing, no stemming. The SurrealDB `custom_analyzer` (lowercase +
snowball) handles normalisation at index time, so raw storage preserves the original casing for
display and filtering while search-time folding ensures recall.

The sidecar method `extract_anchors(memory_id, summary, project?, domain?)` runs concurrently
with the existing `extract_relations` call in the post-create async pipeline. Both share the
same `Visibility::Sensitive` gate — sensitive memories are never sent to the LLM.

---

## Alternatives Considered

- **Query rewriting at recall time (Phase 2-A, IRCoT-lite)** — Deferred, not abandoned.
  Adds LLM cost to every recall call and introduces latency on the critical path. Phase 2-B
  provides write-time signal first; Phase 2-A will be re-evaluated once Phase 2-B's impact on
  recall quality is measured. The eager flag from Phase 2-A planning is preserved.

- **Synonym dictionary / controlled vocabulary** — Rejected. A static dictionary cannot cover
  project codenames, vendor names, or person names — exactly the vocabulary where the gap is
  largest. Requires ongoing maintenance and misses novel entities.

- **Full embedding refresh on a better model** — A different axis: higher-dimensional
  embeddings reduce the _semantic_ gap but cannot bridge the _lexical_ gap in BM25.
  Complementary, not substitutable. Does not help when the query term literally does not appear
  in the summary.

---

## Consequences

### Positive

- Zero recall-time cost — anchors are indexed at write time; BM25 query extension is O(1).
- Concurrent with relation extraction — no sequential latency added to post-create pipeline.
- Existing BM25 infrastructure (SurrealDB analyzer, RRF fusion) reused unchanged.
- Enables future entity-filter UI: `anchor_entities` is a queryable field.
- `--reextract-anchors` admin CLI subcommand allows backfilling existing memories without
  running a full re-embed.

### Negative

- +1 sidecar call per memory write (budget: `maxBudgetUsd: 0.02`, 15 s `AbortController`).
- Anchors are NULL until the sidecar processes the memory (async). BM25 anchor boost is
  unavailable for a short window post-write. The base BM25 + vector path is unaffected.
- Extraction quality depends on summary length and entity clarity. Short or abstract summaries
  may yield zero anchors — this is correct behaviour (no forced extraction).

### Trade-offs Accepted

- Sensitive memories permanently receive no anchor extraction. Entity-faceted recall for
  sensitive memories relies solely on the base BM25 + vector pipeline. Deliberate privacy
  preference, consistent with the typed-relation extraction trade-off in ADR-013.
- Raw storage without normalisation means `anchor_entities` values are case-sensitive when
  used as direct field predicates; search-time folding only applies through the BM25 index.

---

## Anti-Goodhart Note

Anchors are extracted using a generic prompt ("named entities + key noun phrases"). The prompt
does not reference benchmark corpora, recall-test vocabulary, or any specific domain. The
feature exists to support **entity-faceted product use cases** — promise tracking, person
search, future filter UI — not to inflate retrieval scores. Synonym stuffing (injecting
query-side vocabulary as fake anchors) is not what the prompt asks for and would corrupt the
field for its product purpose.

---

## Phase 2-A Status

Phase 2-A (recall-time IRCoT-lite query rewriting behind an `eager` flag) is **DEFERRED** —
not abandoned. It will be re-evaluated after Phase 2-B's signal is measured in production.
The eager-mode flag design from Phase 2-A planning is preserved for that work.

---

## Implementation Map

| File | Role |
|------|------|
| `sidecar/src/index.ts` | `extract_anchors` method; `ANCHOR_SCHEMA` (maxItems 3); `ANCHOR_SYSTEM_PROMPT`; 15 s `AbortController`; `maxBudgetUsd: 0.02` |
| `src/llm/extractor.rs` | `extract_anchors_and_persist`; validates response, persists to `memory.anchor_entities` |
| `src/llm/mod.rs` | Re-exports `extract_anchors_and_persist`; `ExtractAnchorsParams` from sidecar |
| `src/llm/sidecar.rs` | `ExtractAnchorsParams` Rust struct; sidecar call plumbing |
| `src/db/migrations.rs` | MIGRATION_014 — `anchor_entities` field + `idx_memory_anchor_ft` BM25 index |
| `src/db/search.rs` | BM25 query OR-extended: `summary @@ $query OR content @@ $query OR anchor_entities @@ $query` |
| `src/tools/remember.rs` | Post-create async: `extract_anchors_and_persist` spawned concurrently with `extract_and_persist` |
| `src/main.rs` | `--reextract-anchors` subcommand for backfilling existing memories |

---

## References

- ADR-013: Claude Agent SDK sidecar — shared infrastructure (supervisor, circuit breaker, auth, Sensitive gate)
- MIGRATION_014 — `anchor_entities` field + `idx_memory_anchor_ft`
- MIGRATION_012 — `typed_relation` table (parallel Phase 1 work, same post-create pipeline)
