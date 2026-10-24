# Smriti build spec: lean MP/1

**Status:** Build plan, 2026-10-24. Smriti is not deployed, so there is no backward-compatibility constraint.
**Sources:** `smriti-v2-memory-protocol.md` (the MP/1 target contract), `runtime-llm-quality.md` (P0–P3), and the 2026-10-24 survey of memory labs (Supermemory, Mem0, Zep/Graphiti, Letta, Cognee, LangMem, Anthropic, OpenAI, Google, MemOS, Mastra, Hindsight, A-MEM, Agent Zero, Voyage).

## Principles

1. **The raw input is never rewritten.** Every interpretation can be rebuilt from it.
2. **Add-only writes, and automation never deletes.** Mem0 dropped its LLM-decided ADD/UPDATE/DELETE step, partly because of malformed LLM output (a 2026 study measured about 25%). Supermemory keeps superseded facts with an `isLatest` flag. Both landed where we already are.
3. **Storage is cheap; search noise is the real bloat.** Archive everything and keep the searchable index small.
4. **Learn from use, not schedules.** `recall_event` and `kb_feedback` replace SM-2.
5. **The bench decides.** Every stage has a gate.

## Architecture

```
 tools: kb_observe  kb_recall  kb_revise  kb_suppress  kb_feedback   (+ entity/agent/admin)
           │           │          │           │            │
 ┌─────────▼───────────▼──────────▼───────────▼────────────▼─────────────────────┐
 │ memory (Note)  text≤500 · kind? · tags[] · raw content (immutable) ·          │
 │                event_at · valid_from/valid_until · recorded_at ·              │
 │                invalidated_at · state · pinned · confidence? · salience       │
 │ link           in · rel · out · confidence · origin · inferred · valid window │
 │ entity (+alias, + rolling summary)                                            │
 │ recall_event   trace_id · session · actor · query_vec(int8) · ids · ranks ·   │
 │                feedback                                                       │
 │ vectors        int8 1024, rebuildable; hot = active notes only                │
 └───────────────────────────────────────────────────────────────────────────────┘
 background (sleep-time pattern, already ADR-026):
   extraction · entity-summary rebuild · consolidation/lessons · salience fold
```

There are two write paths, following Supermemory's split between memories and documents:

- **Memory path** (the default). Validate, embed contextually, then dedup. A near-duplicate *reinforces* the existing note. Then create, then async extraction.
- **Reference path** (`is_static` documents). Chunk, embed and index only. No extraction, no dedup and no relation inference, which keeps the sidecar's load down.

## What we take from the labs

| # | Idea | Source | Smriti form |
|---|---|---|---|
| L1 | Contextualized chunk embeddings, int8 | Voyage | One `/contextualizedembeddings` call per document with all chunks. Store int8. Benchmark `voyage-context-4` against context-3; its existence and pricing are unverified. |
| L2 | Event time vs transaction time on edges | Zep/Graphiti | `link.valid_from/valid_until`, separate from `recorded_at`/`invalidated_at`. |
| L3 | Contradiction yields a "used to…" fact | Zep (adapted) | Zep rewrites the old fact's text in place; we write a successor note instead, and the old note stays immutable. |
| L4 | Rolling entity summary | Zep | `prior_summary + new_fact → summary`, as a background job. Fed to `kb_entity_get`. |
| L5 | Inferred edges carry low weight until confirmed | Cognee | `link.inferred = true` and `confidence`. Spreading activation discounts them, and `kb_feedback(used)` confirms them. |
| L6 | Lessons judged as success or failure | Google ReasoningBank, EverMind | The consolidator writes procedural notes framed as "worked" or "mistake to avoid", from session outcomes. |
| L7 | Citation lock | Agent Zero | Recall returns evidence spans. A consuming agent may cite only spans it actually received, keyed by `trace_id`. |
| L8 | Confidence on beliefs | Hindsight | Optional `confidence` on fact and hypothesis notes. Asserted claims are kept distinct from grounded ones. |
| L9 | Tokens per recall as a metric | Mem0 | `recall_event.tokens_returned`, reported in `kb_status`. |
| L10 | Multi-scale evaluation | Mem0 BEAM (degrades from 1M to 10M) | The bench runs at 1×, 10× and 100× store size. |
| L11 | Review gate on derived facts | Supermemory "dreaming" | Consolidator output enters as `pending`. It becomes active after a Jev or Sonnet check, or after owner review. |
| L12 | Agentic retrieval (ASMR) | Supermemory | **Parked.** At most an optional fallback for low-confidence queries; never on by default. |

Skipped, with reasons:

- **Letta memory blocks.** This is the caller's job, and `kb_profile` plus pinned notes already cover it.
- **The LangMem taxonomy.** We already have more structure than we need.
- **Mem0's LLM-arbitrated mutation.** It is unreliable.
- **MemOS KV-cache tiers.** Out of scope for an MCP server.

## Taxonomy

`type` and `subtype` become **optional hints**. One soft field, `kind ∈ {event, fact, procedure, intent}`, survives as a ranking boost. Subtypes become tags. The working subtypes become default `valid_until` values (scratch 48h, signal 14d, priority 7d, open_question 30d, hypothesis 7d). `value` and `life_goal` become `pinned`. Importance floors and ceilings are removed, and `importance` is stored as the caller's prior and never mutated. See the table in `smriti-v2-memory-protocol.md` §4.

## Build stages

| Stage | Work | Gate |
|---|---|---|
| **B0 Bench** | Restore or create `bench/`: LoCoMo10 and LongMemEval-S. Report Recall@k by stage (BM25, vector, fused, reranked) and tokens per recall, at three store scales. Add byte accounting to `kb_status`. | 3 runs within ±1 pt, establishing the baseline. |
| **B1 Vector path** | int8 vectors and an id-only scan or HNSW. Contextual document calls (L1). One batch re-embed. | Recall@10 at least the baseline; p50 at least 30% lower. |
| **B2 Truth and taxonomy** | Optional type/subtype, `kind`, and tags. Reinforce instead of reject on dedup. Keep raw `content` immutable. Prune stops mutating importance, and drill stops writing `updated_at`. Remove SM-2. Reference path for `is_static`. | Recall@10 holds; synthetic decay probes at least baseline. |
| **B3 Feedback loop** | `recall_event`, `trace_id`, `kb_feedback`, and salience as a ranking prior. Tokens-per-recall metric (L9). | Recall@1 rises 2 pts on the held-out split, or salience weight stays 0. |
| **B4 Graph quality** | `link` table with an event-time window (L2), inferred weight (L5), rolling entity summaries (L4), and "used to…" successor notes (L3). | Temporal and knowledge-update categories rise. |
| **B5 Lessons and evidence** | ReasoningBank-style lessons (L6), review gate (L11), evidence spans with citation lock (L7), and confidence (L8). | Answer accuracy rises; there is no hallucinated-citation regression. |
| **B6 Tool surface** | Five core tools plus entity, agent and admin tools. Delete legacy names (nothing is deployed). | Contract tests pass. |

Already done on this branch (`a7affac`):

- The P0 retrieval fixes.
- Sensitive memories are no longer blocked from enrichment.
- Sidecar models are configurable.
- The content prefix is added to the embedding text, and `kb_reembed` is fixed to match.

## Blocking input

B0 needs the bench. `/bench/` is gitignored ("separate subproject") and appears to live on the `stark` Linux machine. Either copy it here, or approve building a new harness from the public LoCoMo dataset (CC BY-NC) and LongMemEval.
