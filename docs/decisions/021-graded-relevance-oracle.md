# ADR-021: Graded-Relevance Oracle Schema

**Status:** Proposed — 2026-04-30
**Date:** 2026-04-30
**Extends:** ADR-007 (test architecture), ADR-017 (Voyage reranker)

## Context

The existing eval bench (`bench/src/synth_bench.rs`, `bench/synth/oracle.jsonl`) scores
each probe as a binary hit/miss:

- `must_include_memory_ids` — all listed IDs must appear in top-K.
- `must_not_include_memory_ids` — none listed may appear.
- `null_expected` — recall must return empty.

Binary scoring yields Recall@K and a hit rate, but not nDCG@K or MRR@K. nDCG requires
graded relevance (0–3); MRR requires a single "most relevant" position. Without these,
the retrieval pipeline cannot be tuned quantitatively:

- The Convex Combination weight for BM25 vs. vector in RRF cannot be calibrated.
- Recency/importance score bonuses have no objective loss to minimise against.
- The adaptive reranker thresholds proposed in ADR-022 have no evaluation signal to
  set against.

A naive fix — bootstrapping graded labels from the current pipeline's output ranking —
would contaminate the oracle with the model's own biases (self-similarity contamination).
Labels must precede the system's ranking.

## Decision

Extend the oracle JSON schema with an optional `graded_relevance` field:

```json
{
  "query": "...",
  "must_include_memory_ids": [...],
  "must_not_include_memory_ids": [...],
  "null_expected": false,
  "graded_relevance": {
    "memory:abc123": 3,
    "memory:def456": 2,
    "memory:ghi789": 1
  }
}
```

Grade semantics:

| Grade | Meaning |
|---|---|
| 3 | Highly relevant — directly answers the query |
| 2 | Relevant — on-topic, useful |
| 1 | Marginal — partially related, not the target |
| 0 | Irrelevant — in probe scope but not useful |

Zero-grades are omitted from the map (absence implies 0), keeping the schema compact.

### Rust type

```rust
#[derive(Debug, Deserialize, Serialize)]
pub struct OracleProbe {
    pub query: String,
    #[serde(default)]
    pub must_include_memory_ids: Vec<MemoryId>,
    #[serde(default)]
    pub must_not_include_memory_ids: Vec<MemoryId>,
    #[serde(default)]
    pub null_expected: bool,
    #[serde(default)]
    pub graded_relevance: HashMap<MemoryId, u8>,
}
```

The field is `#[serde(default)]` — existing probes deserialise without it. No
migration of the 2,500-probe synth corpus is required.

### Eval runner additions

When a probe carries `graded_relevance`, the runner computes:

- **nDCG@10** — standard normalised discounted cumulative gain with ideal ordering
  from the same grade map.
- **MRR@10** — reciprocal rank of the first grade-2-or-higher result.
- **Recall@{5, 10, 20}** — fraction of grade-2+ IDs retrieved within K.

Binary hit/miss metrics are always computed regardless of whether graded labels
are present, so existing CI gates are unaffected.

### Label authorship rules

1. Graded probes are hand-labeled by the operator from memory of relevance before
   running recall — not after inspecting ranked output.
2. Bootstrapping from ranked output is explicitly prohibited: the eval script will
   refuse to write `graded_relevance` entries derived from the system's own scores.
3. Initial target: 100–200 probes drawn from real usage sessions, not the synth corpus.
4. Probes are appended to `oracle.jsonl`; the synth corpus is not modified.

## Consequences

### Positive

- nDCG@10, MRR@10, Recall@{5,10,20} available as calibration targets for
  ADR-022 (adaptive reranker) and future RRF weight tuning.
- Zero breaking changes — additive schema, backward-compatible deserialisation,
  existing binary CI gate unchanged.
- Clean evaluation discipline: graded labels precede pipeline runs, eliminating
  self-similarity contamination.

### Negative — must accept

- Hand-labeling 100–200 probes is a one-time manual cost (~2–4 hours).
- Graded set is small by IR standards; statistical power is limited. Treat nDCG
  estimates as directional until the set exceeds 500 probes.
- Keeping graded probes in `oracle.jsonl` alongside synth probes requires a
  careful authorship note in each entry; tooling should warn if a graded probe
  references a memory ID not present in the test DB.

### Trade-offs accepted

- Single file over dual-file maintenance: `oracle.jsonl` holds both probe types.
  Field presence distinguishes them; no secondary index required.
- Grade scale capped at 3 (not 4 or 5) — sufficient for nDCG computation at
  personal-memory scale; avoids labeler ambiguity between adjacent grades.

## Alternatives considered

- **Replace hit/miss entirely with graded labels** — rejected. Loses the existing
  2,500 synth probes and their CI signal. Binary hit/miss is cheap to author
  and catches regressions well; graded scoring is complementary, not a replacement.
- **Separate graded oracle file (`oracle_graded.jsonl`)** — rejected. Dual
  maintenance burden; eval runner must join two files by query string (fragile).
  Single file with optional field is strictly simpler.
- **Bootstrap labels from current pipeline ranking** — rejected. Self-similarity
  contamination: labels derived from the ranked output encode the model's biases
  back into the eval, making future improvements invisible.
- **5-point grade scale (0–4)** — rejected. Labeler ambiguity between grade 3 and 4
  at personal-memory domain is high; nDCG gain over 4-point scale is negligible.

## Related

- ADR-007: Test architecture (eval harness this ADR extends).
- ADR-017: Voyage reranker (ADR-022 adaptive policy depends on graded eval from this ADR).
- ADR-022: Adaptive reranker policy (primary consumer of nDCG@10 from this ADR).
