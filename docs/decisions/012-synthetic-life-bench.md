# ADR-012: Synthetic Personal-Memory Bench ("Arjun Bhatt" Corpus)

**Status:** Accepted
**Date:** 2026-04-22
**Context:** LoCoMo bench measures conversational-memory recall on fictional two-person dialogs with 6.4% corrupted gold (dial481) and a hard ceiling ~0.75. It does not exercise smriti's typed writes, agent authorship, bitemporal recall, visibility scoping, consolidation, retraction, or entity graph. The bench number under-reports capability and mis-targets optimization. smriti is a **personal second memory** for one user writing across years via multiple agents — the evaluation must match that shape.

---

## Decision

Build a **2-year synthetic life corpus** for a fictional persona ("Arjun Bhatt") as the canonical smriti bench. Generate once, reuse forever. Runs across **every MCP tool**, **every type/subtype pair**, **every invariant** (bitemporal, visibility, consolidation, retraction, TTL, dedup, entity graph). LoCoMo stays as a wild-data smoke test only — not the headline number.

---

## Why Synthetic, Not Real User Data

- **Privacy**: benchmarking on Dhruv's actual memories leaks personal data.
- **Oracle**: with a planted dataset, we *know* the ground truth per probe, eliminating judge noise at the retrieval layer.
- **Coverage**: real usage is skewed — synthetic lets us plant every edge case (retractions, consolidations, visibility traps) deliberately.
- **Reproducibility**: regeneratable from seeds, swappable, versionable in git.
- **Non-overfit**: Arjun is Dhruv-adjacent but distinct (different name, details) so smriti doesn't overfit to a single real user's quirks.

---

## Scope (Locked)

| Dimension | Target |
|---|---|
| Persona | 1, "Arjun Bhatt", 28, Bangalore software engineer |
| Timeline | 2 years (730 days), 2026-04-23 → 2028-04-22 |
| Agents | 7 (coding, journal, research, home, health, finance, learning) + 1 retired mid-timeline |
| Entities | 80+ across Person/Company/Place/Product kinds |
| Memories | 15 000 – 20 000 typed writes |
| Probes | 2500 across 15 categories |
| Mutations | 150 retractions, 30 supersessions, 100 consolidations |

**Coverage invariants** (non-negotiable):

- ≥ 5 memories per `(type, subtype)` pair → 115+ anchor memories
- ≥ 1 probe exercising each of 25 MCP tools
- ≥ 5 memories per of the 7 agents (and 10+ for primary agents)
- Every invalidation reason exercised: superseded, retracted, expired, archived, consolidated, deprecated
- Every visibility mode (Private, Shared) used meaningfully
- Every entity kind populated
- Every relation enum tested (updates, extends, derives, relates, retracts)

---

## Generation Pipeline

Layered, each layer owns a schema. Code under `bench/python/gen_*.py`. Data under `bench/synth/`.

| Layer | Generator | Tool | Output |
|---|---|---|---|
| L1 Persona | `gen_persona.py` | Sonnet 4.6 (claude-agent-sdk) | `persona/arjun.yaml` |
| L2 Agents | `gen_agents.py` | Sonnet | `agents/agents.jsonl` |
| L3 Entities | `gen_entities.py` | Sonnet | `entities/{people,places,companies,products}.jsonl` |
| L4 Weekly arcs | `gen_arcs.py` | Sonnet | `timeline/arcs.jsonl` (104 entries) |
| L5 Daily events | `gen_events.py` | Gemma 4 E4B | `timeline/events-YYYY-MM-DD.jsonl` |
| L6 Typed memories | `gen_memories.py` | Gemma 4 26B MoE (strict schema) | `memories/memories-YYYY-MM.jsonl` |
| L7 Mutations | `apply_mutations.py` | Rules + Sonnet (wording) | `memories/mutations.jsonl` |
| L8 Probes | `gen_probes.py` | Sonnet | `probes/probes-{category}.jsonl` |
| L9 Paraphrases | `paraphrase_probes.py` | Gemma E4B | `probes/variations.jsonl` |
| L10 Oracle compile | `compile_oracle.py` | Python + Gemma E4B validator | `oracle/oracle.jsonl` |

Principles:

- **Sonnet for creative + coherent work** (persona, arcs, retraction wording, probe design).
- **Gemma local for bulk / schema-strict work** (events, memories, paraphrases, validators, judge).
- **Deterministic glue** (oracle compilation, mutation planting) stays pure Python.

Resource budget:

- **Claude Max plan**: ~500-700 Sonnet calls over 3-4 days (within rate limits).
- **Local GPU (RTX 4060)**: ~30-50 hours across overnight runs (~1 week).
- **$ cost**: zero for Gemma; Max-plan flat rate for Sonnet.

---

## Probe Battery

2500 probes, 15 categories, balanced:

| Category | Count | Invariant Tested |
|---|---|---|
| 1-hop factual | 300 | baseline kb_recall |
| 2-hop via entity graph | 250 | spreading activation |
| Temporal snapshot (recall_at) | 300 | bitemporal correctness |
| Preference evolution | 150 | supersession-aware recall |
| Promise / action_item tracking | 150 | status-over-time |
| Contradiction (post-retract) | 150 | retraction-aware |
| Entity-anchored | 250 | person-filter precision |
| Project-scoped | 150 | project filter |
| Domain-scoped | 100 | domain tag filter |
| Visibility leak | 150 | privacy enforcement |
| Consolidation addressability | 150 | post-merge IDs |
| Dedup-warn behavior | 100 | warn-band decisions |
| Null-answer (anti-fabrication) | 200 | "I never told you X" |
| Cross-agent visibility | 50 | agent A vs agent B views |
| TTL-expiry | 50 | working-memory lifecycle |

Each probe record:

```json
{
  "id": "probe-0001",
  "question": "What did Arjun decide about the reranker?",
  "expected_answer": "Go with BGE-reranker-v2-m3 for v1",
  "category": "1-hop-factual",
  "tools_exercised": ["kb_recall"],
  "invariants": ["visibility"],
  "evidence_memory_ids": ["mem:abc"],
  "as_of_date": null,
  "author_filter": null,
  "project": "smriti",
  "difficulty": "easy"
}
```

---

## Rust Bench Harness

`bench/src/synth_bench.rs` — new binary, sibling of `locomo.rs`.

**Three ingest modes** run end-to-end:

1. **Raw-turn** (baseline, LoCoMo-style) — dumps events as memories without typing.
2. **Oracle** — loads `memories.jsonl` directly via the real `kb_remember` path (upper-bound ceiling).
3. **Skill-mediated** — Sonnet reads events and writes via kb_remember using the production ingestion skill (the real-world measurement).

Each mode reports:

- Per-category accuracy (precision, recall, F1 against oracle)
- Per-tool exercise count + success rate
- Per-subtype recall rate
- Per-invariant pass rate (visibility, TTL, bitemporal, consolidation, retraction)
- Latency histograms
- Export/import roundtrip diff (must be zero)

**Single headline number is meaningless**; the matrix is the output.

---

## Non-Goals (Explicit)

- Not competing with Mem0 / Zep on their benchmarks.
- Not trying to hit 0.9+ on LoCoMo (unreachable; corrupted gold).
- Not generating infinite scale — 18k memories fits one laptop for iteration.
- Not production-grade data augmentation for training.

---

## Risk / Open Items

- **Sonnet rate limits** during generation — mitigate by batching + multi-day run.
- **Gemma 26B schema drift** — mitigate by validator retries, per-batch stats alerts.
- **Bench-overfit risk** — mitigate by keeping LoCoMo as dissonant wild smoke test; regenerate Arjun periodically with rotated seeds.
- **Persona realism** — Sonnet-generated personas can be stereotyped. Human review of persona + entities before committing.

---

## Acceptance

- P1 ADR + SPEC + COVERAGE + gen_common committed.
- P2 Persona + agents + entities generated and human-reviewed.
- P3 Timeline (730 days, 10-20 events/day) generated.
- P4 Memories (15-20k) + mutations applied. Coverage matrix passes (all cells ≥ threshold).
- P5 Probes (2500) + oracle compiled and spot-reviewed.
- P6 Rust harness produces the per-category matrix across 3 ingest modes.
- P7 Smriti version tagged; future changes tracked against this matrix.

Superseded-by: none yet.
