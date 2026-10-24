# Grounding

## Digest
I now have enough grounding. Here is the digest.

---

**SMRITI GROUNDING DIGEST — for taxonomy/protocol re-evaluation**

**Data model & storage footprint per memory** (`docs/data-model.md`; schema `src/db/migrations.rs:132-376`)

Each `memory` row: `summary` (10-500 char, required, always injected), `content` (optional, ≤500KB, `validation.rs:91` `MAX_CONTENT_BYTES`, fetched only on `kb_drill`), `memory_type`+`subtype` (23-subtype taxonomy, string fields, no DB enum), `source`, `source_ref`, `project` (option), `people: array<string>`, `domain: array<string>`, `importance: int` (floor/ceiling per subtype), `status` (derived hint), `tags: array<string>`, `access_count: int`, 4 bitemporal timestamps (`event_at`, `created_at`, `updated_at`, `valid_until`, `invalidated_at` + `invalidation_reason`), `expires_at` (TTL), `visibility`, `source_agent`/`session_id`, `author: option<record<agent>>` (`migrations.rs:267`), `is_static: bool` (`:319`), SM-2 fields `ease_factor`/`review_interval`/`next_review_at` (`:225-227`), `anchor_entities: option<array<string>>` ≤3 items (`:375`), and `vector: option<array<float>>` (`:216`).

Vector cost: local fastembed = 384×f32 = 1536B/row; Voyage voyage-context-3 = 1024×f32 = 4096B/row (`architecture.md` embeddings section). 10 B-tree/BM25 indexes on `memory` alone (`migrations.rs:157-167,268,280-281,320,332,376`) plus `memory_bitemporal_idx` composite. Long content (≥2000 chars, `chunking.rs:27` `CHUNK_THRESHOLD`) is split into full **sibling memory rows** — each chunk duplicates the entire schema (own vector, own indexes, own bitemporal fields) and links to parent via `chunk -[extends]-> parent` (`chunking.rs:341`, `data-model.md` Graph Edges). This is the single biggest unmeasured space cost: a 20KB transcript can become 10+ full-weight memory rows, each independently indexed, vectored, and subject to SM-2/TTL/archive logic.

Edges: `involves`, `assigned`, `updates`, `extends`, `derives`, `relates` (auto-link, cosine thresholds 0.75/0.85/0.90, max 8/memory, `data-model.md` Auto-Link), `retracts` (Migration 008), `typed_relation` (LLM-extracted, confidence+provenance, Migration 012), `entity_alias` (separate SCHEMAFULL table, not RELATE, Migration 021/ADR-030). Ledger tables: `consolidation_attempt` (Migration 018), (planned) `judge_verdict` (Migration 016, not yet built).

STATUS.md measured footprint: 281MB on-disk at the current corpus size, flat across the ADR-022/031 benches (rerank/MMR don't change storage).

**Lifecycle — what actually deletes vs. hides**

Nothing hard-deletes (`architecture.md` "Rule: never hard-delete"). `BITEMPORAL_ACTIVE` predicate (`invalidated_at IS NONE AND (valid_until IS NONE OR valid_until > now())`) filters every read site; `status='archived'` is a derived index hint kept in sync, never authoritative. TTL (`validation.rs:75` `ttl_duration`) hard-expires `working/*` subtypes (scratch 48h, priority 7d, open_question 30d, signal 14d, hypothesis 7d) by setting `invalidated_at`/`Expired` — row stays in DB. SM-2 decay (`prune.rs:80-147`) lowers `importance` on unreviewed-past-due memories (ease -0.2 floor 1.3, interval reset to 1), skipping importance≥5, `decision`/`value`/`life_goal`, and `working/*`. `domain_decay_rate` (`prune.rs:21`) computes a domain-specific multiplier (health/fitness 0.5×, philosophy/personal 0.25×) but **is dead code — never called** (STATUS/D10, confirmed by grep: no call site outside its own definition). Archive threshold: importance falling to configured min triggers archive (sets `invalidated_at`/`status`). Supersession (`updates_id`), retraction (`kb_archive reason=retract`, writes `retracts` edge), consolidation merges (`consolidation_attempt` ledger, Migration 018) — all soft-invalidate only. Net effect: the DB is monotonically growing; "forgetting" is purely a read-time filter, so space complexity is unbounded by design unless something eventually compacts/exports archived rows (nothing currently does).

**What the prior proposal already decided — do not redo**

`docs/proposals/runtime-llm-quality.md` (decided, 2026-10-24) already diagnosed the system as retrieval-bound (D1: 63.4% acc, 76.5% conditional-on-hit, 44% miss rate) and NOT LLM-usage-bound, and laid out a full P0-P3 roadmap + model-tier routing table (Jev for classification/judging, Haiku for generation, Sonnet only for consolidation synthesis, Opus/Fable offline-only). It already decided: Sensitive visibility gates reads only, never embedding/enrichment (owner decision, commit 91e410b); judges are additive-only (`CreateEdge|RaiseAlert|Tag|SkipMerge|AllowMerge`, no Archive/Retract variant — compiler-enforced); pool_size config added (default still 50, `config.rs:254`, flip to 150 is P0.5, not yet flipped); content-prefix embedding mechanism built (`validation.rs:152-174` `EMBED_CONTENT_CHARS`, default 0/off — P0.8 code done, not enabled); `primary_cap` reservation removal (P0.6) and `should_rerank` Semantic-only fallthrough removal (P0.7) still pending in `search.rs:1161-1167`/`:865-909`; sidecar model is still hardcoded `claude-sonnet-4-5` (`sidecar/src/index.ts:10,240,308,383`) — P0.4 config plumbing not yet landed. **Do not re-debate**: architecture decisions in `docs/decisions/*.md` (31 ADRs), especially bitemporal model (ADR-011), judge additive-only invariant, Jev as primary judge engine with LocalJudge as vendor exit, rejection of HyDE/stacked-reranker/auto-upgrading-PII/per-recall reconsolidation spawns (all listed under "Rejected ideas").

**Top 10 places the system lags or carries unmeasured complexity**

1. **Retrieval pool truncation before ranking** — `search.rs:855-856` pool capped at `max(limit,50)` (still 50 by default); gold evidence ranking >50 in both BM25 and vector is permanently unreachable (D2).
2. **`primary_cap` reserves answer slots for graph neighbours** — `search.rs:1161-1167`, 3 slots taken from the final result regardless of whether spreading activation found anything useful (D2/P0.6 pending).
3. **Embed text starvation** — `remember.rs:22,178` embeds only `[type/subtype][project][domains]+summary`; content prefix mechanism exists (`validation.rs:161`) but defaults to 0 chars, so no content reaches the embedding that matters most (candidate recall), even though the Voyage reranker reads 3200 chars at rank time only (D3).
4. **`should_rerank` policy gap** — `search.rs:865-909` only forces rerank for `Semantic` type queries; episodic/procedural/prospective short queries never get reranked (D4), and the system has no measured Recall@1 gain data to justify the current heuristic thresholds.
5. **Typed-relation extraction is structurally broken** — `sidecar/src/index.ts:148-245` sends no candidate memory IDs to the model, so extracted `to` fields can't resolve to real edges; `extractor.rs:64` trusts the model's self-reported confidence with no calibration (D5). This directly undermines spreading activation (ADR-023), which depends on `typed_relation` edges existing.
6. **Chunking multiplies storage with no space-complexity accounting** — every memory ≥2000 chars (`chunking.rs:27`) becomes N full schema+vector+index rows (`chunking.rs:341`); no measurement exists of how much of the 281MB/row-count is chunk duplication vs. primary content, and no consolidation/compaction path for chunk rows exists.
7. **`recall_at` is a second, weaker pipeline** — `search.rs:1607-1870` lacks person filter, alias expansion, and spreading activation that `recall()` has (D6); any code reusing one path's logic risks drifting further from the other.
8. **Measurement infrastructure is broken/absent** — `bench/` directory is gitignored and missing from this checkout (D8); the "LoCoMo 63.4%" figures the whole proposal is built on come from n=196 QAs / samples=1 (±7pt CI), so most prior "wins" (MMR default-on, ADR-022 rerank) shipped unmeasured against noise-floor data.
9. **Dead/unwired tunables masquerading as features** — `domain_decay_rate` (`prune.rs:21`) computed but never called; sidecar model hardcoded despite a routing table that assumes configurability (D9/D10). Signals an unmeasured-complexity pattern: config surface grows (importance floors, visibility tables, dedup threshold, decay rates) faster than anything validates it's earning its keep.
10. **Taxonomy enforcement has no adaptive feedback loop** — 23 subtypes with hardcoded floor/ceiling tables (`data-model.md` Importance table) are caller-classified and server-validated, but nothing in the retrieval/consolidation pipeline measures whether a given type/subtype actually predicts retrieval value or access patterns — the taxonomy's fitness for the "adaptive, learns what matters" owner goal is currently unmeasured, same blind spot as D8 but at the schema-design level rather than the ranking level.

**Files most relevant to a taxonomy re-evaluation**: `docs/data-model.md`, `src/db/migrations.rs:132-376` (schema), `src/types/enums.rs` (1014 lines — type/subtype enums + validation pairing), `src/db/validation.rs` (importance floors/ceilings, TTL, contextual-text/embed-text construction), `src/tools/remember.rs` (write path, 556 lines), `src/db/search.rs` (2312 lines — all retrieval stages), `src/db/prune.rs` (215 lines — SM-2/TTL/archive), `src/db/consolidation.rs` (821 lines), `src/db/chunking.rs` (673 lines).

## Voyage
# Voyage AI Research Brief (for Smriti memory server, ~10^4–10^6 memories)

## Model lineup (2026)

**Embeddings**, priced per million tokens, with context/batch caps ([docs.voyageai.com/pricing](https://docs.voyageai.com/pricing), [embeddings API reference](https://docs.voyageai.com/reference/embeddings-api)):

| Model | $/M tok | Free tier | Max tokens/request list |
|---|---|---|---|
| voyage-4-lite | 0.02 | 200M | 1M |
| voyage-4 | 0.06 | 200M | 320K |
| voyage-4-large | 0.12 | 200M | 120K |
| voyage-context-4 | 0.12 | 200M | — |
| voyage-3.5-lite | 0.02 | none (older) | 1M |
| voyage-3.5 | 0.06 | none (older) | 320K |
| voyage-context-3 | 0.18 | none (older) | 32K/doc per [LiteLLM](https://docs.litellm.ai/docs/providers/voyage), unverified against Voyage's own page |
| voyage-code-3 | 0.18 | conflicting (page lists it both with and without 200M free — unresolved) | 120K |
| multimodal-3.5/3 | 0.12 text + $0.60/B pixels | 200M text + 150B pixels | — |

voyage-4 family launch date: third-party listings disagree (Jan 15 vs Jul 27, 2026) — **unverified**, I could not reach Voyage's own launch post, only [Vercel](https://vercel.com/ai-gateway/models/voyage-4-large/about) and [OpenRouter](https://openrouter.ai/voyageai/voyage-4-large-20260727) mirrors. voyage-4-large is reportedly MoE-architecture with ~40% lower serving cost than dense peers of similar quality — vendor-reported, unverified independently.

**Rerankers** ([reranker docs](https://docs.voyageai.com/docs/reranker)): rerank-2.5 ($0.05/M), rerank-2.5-lite ($0.02/M), both 32K context, 8K query cap, 200M free tokens. Reranker billing = `(query_tokens × num_docs) + sum(doc_tokens)` — scales with candidate-set size, matters a lot for cost modeling.

**Contextualized chunk embeddings** — voyage-context-3/4 ([API reference](https://docs.voyageai.com/reference/contextualized-embeddings-api), [MongoDB repost of launch blog](https://mongodb.com/company/blog/product-release-announcements/voyage-context-3-focused-chunk-level-details-global-document-context), original at blog.voyageai.com/2025/07/23/voyage-context-3/ — not fetched directly, **unverified**). Each chunk's vector is conditioned on the whole document in one call, no manual context-prepending needed; claimed +12–14% over OpenAI-v3-large on chunk/doc retrieval (vendor's own RTEB-style benchmark, unverified).

## Core knobs

- **`output_dimension`** (Matryoshka, docs don't use that word but describe the same nesting): 2048 / 1024 (default) / 512 / 256, on voyage-4-large/4/4-lite, voyage-3-large, voyage-3.5/3.5-lite, voyage-code-3.
- **`output_dtype`**: float (default, highest precision), int8, uint8, binary, ubinary (1/8 the length, bit-packed) — same model set as above.
- **`input_type`**: null / `query` / `document` — prepends a fixed instruction string per side; embeddings with and without it are stated compatible, so you can backfill later.
- **Batch API**: 33% discount, 12-hour window, free credits don't apply ([pricing page](https://docs.voyageai.com/pricing)).
- **Files API**: $0.05/GB/month, 30-day auto-delete.
- **Quantization quality**: Voyage's own docs only state float = highest accuracy, no retention numbers published there. Third-party figures (unverified, not Voyage's own blog): Vercel claims 1024-dim int8 loses 0.31% vs 2048-dim float; MongoDB claims 95%+ recall retention at 1024-dim with proper `numCandidates`, scalar quant ~90%+ retention, binary up to 24x RAM reduction but needs a float rescoring pass.
- **Rate limits**: conflicting sources — Voyage's rate-limits page says flat 300 RPM/1M TPM (embeddings), 100 RPM/2M TPM (rerank); MongoDB's tier table shows per-model, per-tier numbers (e.g. voyage-4: 8M TPM at tier 1, doubling/tripling at tiers 2/3). Free trial without card: 3 RPM/10K TPM. Treat the flat numbers as possibly stale; check your dashboard.

## Recommendations for Smriti (10^4–10^6 memories, local server + cloud embedder)

1. **Embed model**: voyage-context-4 ($0.12/M) if contextualized chunking fits the memory-as-chunk model (a memory's embedding benefits from project/domain context the way a doc chunk benefits from its document) — otherwise voyage-4 ($0.06/M) is the plain default. At 10^6 memories × ~100 tokens/summary ≈ 10^8 tokens one-time, well inside the 200M free tier even at full price after that (~$6–12 total). This is not a scale problem; don't over-engineer for cost here.
2. **Asymmetric input_type is free and already the right call** (matches your `embed_query()`/`embed_document()` split) — no reason to use a separate cheap-docs/expensive-query model pair; Voyage states same-family embeddings with/without `input_type` stay compatible, so this is low-risk.
3. **Dimension/dtype for storage**: at 10^6 memories, 1024-dim float32 is 1024×4 = 4KB/vector = ~4GB total — trivial for local SurrealDB. Reducing to 512-dim or int8 buys little and costs recall; reserve binary/int8 quantization only if you cross into 10^7+ or move off local disk. Don't quantize prematurely — the space savings aren't needed at this scale, per the sizing math above (cf. ADR-031/pipeline notes on storage already in repo).
4. **Rerank**: rerank-2.5-lite ($0.02/M) for the Adaptive Rerank stage is cheap enough to leave on by ADR-022 default; cost scales with candidate count (query×docs formula above), so cap candidate-set size before reranking rather than trimming rerank usage itself.
5. **Batch API**: not useful for live write/recall paths (needs sync latency); reserve for bulk `kb_reembed()` runs on fingerprint-mismatch — the 33% discount matters there, 12h window is fine for an offline reembed job.
6. **Open gap, flag don't fix here**: Voyage's own docs don't publish quantization-retention numbers or a resolved rate-limit tier table — if Smriti ever needs SLA-level rate planning, pull this from the live Voyage dashboard rather than these secondary sources.

Sources: [docs.voyageai.com/pricing](https://docs.voyageai.com/pricing), [embeddings API reference](https://docs.voyageai.com/reference/embeddings-api), [contextualized embeddings API](https://docs.voyageai.com/reference/contextualized-embeddings-api), [reranker docs](https://docs.voyageai.com/docs/reranker), [rate limits](https://docs.voyageai.com/docs/rate-limits) (flat figures, possibly stale) vs [MongoDB rate-limits mirror](https://www.mongodb.com/docs/voyageai/management/rate-limits/) (tiered figures), [flexible dimensions/quantization docs](https://docs.voyageai.com/docs/flexible-dimensions-and-quantization), [LiteLLM Voyage provider](https://docs.litellm.ai/docs/providers/voyage), [Vercel voyage-4-large](https://vercel.com/ai-gateway/models/voyage-4-large/about), [OpenRouter voyage-4-large](https://openrouter.ai/voyageai/voyage-4-large-20260727), [MongoDB voyage-context-3 launch repost](https://mongodb.com/company/blog/product-release-announcements/voyage-context-3-focused-chunk-level-details-global-document-context).

## Reference systems
**Reference designs: data model, bloat, updates, forgetting, and what to borrow**

**1. MCP reference server (modelcontextprotocol/servers, src/memory)**
- Source: https://github.com/modelcontextprotocol/servers/tree/main/src/memory
- Data model: entities `{name, entityType, observations[]}`, where each observation is one atomic string. Relations are directed and active-voice (`from`, `to`, `relationType`).
- Tools: `create_entities`, `create_relations`, `add_observations`, `delete_entities|observations|relations`, `read_graph`, `search_nodes`, `open_nodes`. There is no update tool. Changes are made by adding or deleting observations.
- Bloat control: duplicate entity names are skipped, and duplicate relations are skipped. Deletes are idempotent.
- Storage: one JSONL file with a `.lock` file and a 60s lock timeout.
- Gaps: nothing is ever superseded or time-stamped, so a changed fact sits next to its old version with no way to tell which is current. Nothing expires, and there is no provenance.
- Borrow: atomic observations, the separation of entities, observations and relations, and idempotent writes. Skip the lack of time and provenance.

**2. Supermemory**
- Sources: https://supermemory.ai/docs/concepts/graph-memory , https://supermemory.ai/blog/memory-engine , https://supermemory.ai/blog/supermemory-vs-mem0.md
- Model: raw documents are kept separately from the memories extracted from them. One input can yield several facts, and each fact is linked into a graph. New facts are linked to older ones as updates or extensions.
- Versioning: an `isLatest` flag marks the current fact, and history is kept. Forgetting is soft: a fact drops out of retrieval, but the source is not necessarily purged. Decay is described as gradual fading by recency and relevance, with no published thresholds.
- Unverified: the chunking details from the brief (sentence boundaries, about 200-character overlap, per-type "Super RAG") did not turn up in the pages I could read. Treat them as unconfirmed.
- Borrow: keep the raw source separate from derived facts, which makes re-extraction possible and nothing is lost. Use an `isLatest`-style flag instead of deleting.

**3. Mem0**
- Sources: https://mem0.ai/blog/mem0-the-token-efficient-memory-algorithm , https://docs.mem0.ai/migration/platform-v2-to-v3.md , https://pypi.org/project/mem0ai/2.0.13/ , original paper Chhikara et al., arXiv, 28 Apr 2025 (I did not locate the arXiv ID).
- The original design ran an LLM extraction pass, then ADD/UPDATE/DELETE/NOOP reconciliation against similar existing memories.
- The newer design, per Mem0's docs and blog, is a single LLM call that only adds, with no UPDATE or DELETE. A changed fact is stored as a new entry with temporal context, and both versions are kept. The vendor-reported scores are LoCoMo 91.6 and LongMemEval 93.4, but these are self-reported, and the open-source SDK may score lower than the managed platform.
- Borrow: append-only writes with time context. This avoids the write-time LLM cost of reconciliation and avoids losing history. The reconciliation step can move to read time or to a background pass.

**4. Zep / Graphiti**
- Sources: https://arxiv.org/pdf/2501.13956.pdf , https://mintlify.com/getzep/graphiti/concepts/temporal-model
- Bi-temporal edges: `valid_at` and `invalid_at` (when the fact was true in the world), plus transaction time (when the system learned it).
- Contradictions set `invalid_at` on the old edge and never delete it. History is preserved, and retrieval filters to currently valid edges.
- Reported results are the authors' own. Graphiti needs an LLM to extract entities and edges on every write, which is costly.
- Borrow: `valid_from` / `valid_until` / `recorded_at` on every memory. "Current" becomes a query filter, not a deletion. The Smriti `invalidated_at` field already does most of this.

**5. Letta (formerly MemGPT)**
- Sources: https://docs.letta.com/concepts/memory-management , https://docs.letta.com/guides/agents/architectures/memgpt , https://docs.letta.com/guides/agents/architectures/sleeptime
- Core memory: small, always-in-context blocks that the agent edits with `core_memory_append` and `core_memory_replace`. Archival memory: an external vector store searched by tool call. Recall: conversation history, summarized and searchable.
- Sleep-time/"dreaming": background subagents review recent conversations and update memory outside the active turn. They can run after a set number of steps or on compaction. The docs mention an optional review step that costs more tokens. They don't state a latency tradeoff.
- Borrow: a bounded, always-loaded core block (it keeps the context small), a separate archive for everything else, and an offline consolidation job. That job is the natural place for reconciliation, which keeps the write path cheap.

**What this means for Smriti**
- The core problem is not the number of types. The 5-types / 23-subtypes taxonomy forces the caller to classify each write, and the reference systems avoid that. Every reference system I checked either uses no types or a few generic ones, and gets its structure from time and provenance instead. Consider collapsing to a small set of kinds plus a `valid_from`/`valid_until`/`recorded_at` triple and a `source_id` link.
- Keep the raw input immutable, and derive facts from it. This is the cleanest path to "nothing lost, everything recoverable," and it lets re-extraction happen when models improve.
- Use append-only writes with time context, as Mem0 and Graphiti do. Move reconciliation into an offline consolidation job, as Letta does. This keeps write cost low and avoids bloat from duplicates.
- Forgetting should be soft. Use a flag or a validity window, and only purge derived copies on an explicit request.
- Use the Voyage embedding model only at write time for facts and at read time for queries, and rerank the top results. This matches the asymmetric `embed_document`/`embed_query` split already in the codebase.
- Not verified here: Voyage contextualized-embedding costs, and whether Supermemory's chunking numbers hold. Check both before building on them.

# Team designs

## Team cognition
# Cognition team — memory-as-brain, applied to Smriti

## 1. Diagnosis (cognitive lens)

Smriti collapses two memory systems the brain keeps apart. Complementary Learning Systems (CLS) says the hippocampus stores fast, sparse, verbatim episodes; the neocortex slowly extracts schematized facts from replay. Smriti has one `memory` row for both, and asks the **caller** to decide at write time which it is (5 types / 23 subtypes). That is asking the hippocampus to pre-decide what the neocortex will learn. Consequences:

- **Pattern separation is inverted.** Dentate gyrus keeps near-identical episodes distinct; Smriti's dedup (cosine >0.92 reject) *discards* them. Two near-identical observations on different days are evidence, not noise. Dedup belongs on derived facts, not observations.
- **Reconsolidation has no channel.** Recall in the brain makes a trace labile and re-strengthened (testing effect: retrieval strengthens more than re-exposure). Smriti only bumps `access_count` on `kb_drill`; recall results never feed back. Nothing in the system learns "what matters."
- **SM-2 is a study-scheduling metaphor, not a memory mechanism.** `ease_factor / review_interval / next_review_at` model a *learner* reviewing flashcards; a store has no review. It then drives `archive_below_threshold`, i.e. forgetting-as-erasure via decay — the exact thing the brain does *not* do. `domain_decay_rate` is dead code for the same reason: nobody could say what it should drive.
- **Schemas are broken.** Entities + typed relations are the schema layer that makes consolidation fast and completion (spreading activation, ADR-023) work. Typed-relation extraction cannot resolve targets (sidecar sends no candidate IDs), so the schema graph is mostly empty.
- **Bloat sits in chunking, not in rows.** A ≥2000-char memory becomes N full rows, each with lifecycle, 10 indexes, SM-2 fields, and a vector. Chunks are sub-trace structure, not memories.
- **Working memory is modelled as a store type**, but it is a *lifecycle* (bounded lifetime), and prospective memory is a *cue* (trigger + intent), not a kind of content.

## 2. Target design

**Units** (one table, one `kind` column):

| kind | brain analogue | fields that matter |
|---|---|---|
| `trace` | hippocampal episode | immutable `content`, `summary`, `observed_at` (transaction), `happened_at` (event), `author`, `visibility` |
| `fact` | neocortical schema entry | `summary`, `valid_from`, `valid_until`, `recorded_at`, `sources[] → trace`, `strength` |
| `intent` | prospective cue | fact + `trigger` (time or cue text), `done_at` |

Common: `tags[]` (free, derived), `importance` (one caller prior, 1–5), `pinned` (replaces `is_static` + floors), `ttl` (optional, replaces `working/*`), `status`, `invalidated_at/_reason`, `vector`.

**New tables**
- `chunk {trace, ord, text, vector}` — no lifecycle, no importance, one index. Embedded with **voyage-context-4** so each chunk vector is conditioned on the whole trace (contextualized embedding replaces manual `[type][project]` prefixing for long content).
- `access_event {memory, request_id, rank, used: bool, at}` — append-only, ~50 B/row, folded hourly into `strength` and then compacted.

**Strength** (replaces SM-2): ACT-R base-level activation, `B = ln Σ t_j^-d` over access events (d≈0.5), plus importance. It is a *ranking prior* fused after rerank, never a filter. This gives spacing and testing effects for free: repeated, spaced retrievals raise B; neglect lowers it toward, but never through, zero.

**Flows**
- *Encode*: `OBSERVE` → validate → embed → create trace (no dedup, no classification). Async: entity link, chunk, tag suggestion (Jev classifier, P1).
- *Replay (sleep)*: the existing `auto-consolidator` (ADR-026) is the *only* writer of derived state: merges near-duplicate traces into facts (`derives` edges), resolves supersession, extracts typed relations **with candidate IDs**, assigns tags, recomputes strength. Prioritized replay: process by `importance × recent access` first.
- *Recall*: BM25 + vector over traces+facts+chunks → pool 150 → rerank-2.5-lite → fuse `strength` → graph completion → emit `access_event` for every returned id (fire-and-forget).
- *Feedback*: caller optionally says which ids were used / wrong; folds into strength and tag fitness.

## 3. Delete or simplify

| Delete | Why |
|---|---|
| 23 subtypes as required input | Taxonomy is an *output* of consolidation; keep as optional hint → `tags` |
| Importance floors/ceilings table | One prior + `pinned` |
| SM-2 fields, `sm2_decay`, `archive_below_threshold`, `domain_decay_rate` | Forgetting is competition, not erasure |
| Dedup reject on traces | Pattern separation; merge in replay instead |
| Chunk-as-memory-row | `chunk` sub-table |
| `kb_recall_at` as a second pipeline | `as_of` parameter on recall |
| `kb_promote` | `FEEDBACK(pin)` |
| `working` type + 5 subtypes | `ttl` lifecycle param |
| 10 memory indexes | Target 5: FTS(summary), vector, bitemporal composite, `(kind,status)`, `author` |

Keep: bitemporal model (ADR-011), additive-only judges, entities/aliases, typed relations, Voyage asymmetric input types, never-hard-delete.

## 4. Guarantees

- **No forgetting**: traces are immutable and append-only; every fact carries `sources[]` (provenance closure), so any derived state is regenerable from traces with a better model. Suppression (`supersede/retract/expire/merge`) only closes a validity interval; `INSPECT` always returns suppressed items.
- **No bloat**: exactly one row per observation; facts are bounded by consolidation (N traces → 1 fact, duplicates merge, superseded facts stop being re-derived); chunks carry no lifecycle; access events compact to one float. Growth is O(observations) with a small constant, not O(observations × chunks × indexes).
- **Adaptivity**: `access_event` is the single feedback channel. Strength learns *which memories* matter; tag-fitness (hit rate per tag in replay stats) learns *which categories* matter — the taxonomy becomes measurable instead of asserted.

## 5. Cost and space (10^5 memories, local)

- Trace row ≈ 0.5 KB metadata + content (avg 2 KB, ≤500 KB) + 4 KB vector (1024 f32) ≈ **650 MB**; int8 vectors → 350 MB. Chunk rows add ~4.2 KB each, only for the ~10% of traces that are long.
- Embedding: 10^5 × ~150 tok = 15 M tokens, inside Voyage's 200 M free tier; full reembed ≈ $2 at voyage-context-4 list price, 33% less via Batch API.
- Rerank: 150 candidates × ~120 tok + query ≈ 20 K tok/recall ≈ $0.0004 (rerank-2.5-lite); 1,000 recalls/day ≈ $0.40/day.
- Access ledger: 10^6 events ≈ 50 MB before hourly compaction; steady state <5 MB.

## 6. Staged migration (reuses P0–P3)

1. **S0 — finish P0** as decided (pool 150, cap removal, rerank policy, sidecar config, content prefix).
2. **S1 — feedback channel** (Migration 022): `access_event` table + `strength` float; recall emits events; hourly fold in the existing drain batch (the "hourly batch" the proposal already chose over per-recall reconsolidation). Remove `archive_below_threshold` from `run_all`; stop writing SM-2 fields (keep columns, no backfill).
3. **S2 — taxonomy softening**: `subtype` optional; P1 Jev classifier writes `tags`; floors/ceilings → `pinned`; `working/*` → `ttl`. Existing rows map mechanically (episodic→trace, semantic/procedural→fact, prospective→intent, working→trace+ttl).
4. **S3 — chunks** (Migration 023): `chunk` table, voyage-context-4, `kb_reembed` backfills; old chunk rows marked `merged` into parent.
5. **S4 — single recall pipeline**: `recall_at` becomes `recall(as_of)` (closes D6).
6. **S5 — replay as sole reconciler**: extend ADR-026 consolidator with candidate-ID relation extraction (fixes D5), dedup-to-fact merging, tag-fitness stats.

## 7. The fundamental memory protocol (one page)

**Units**
- `Trace` — an immutable observation: `content, observed_at, happened_at, author, visibility`.
- `Fact` — a derived assertion: `text, valid_from, valid_until, recorded_at, sources[]≥1, strength`.
- `Intent` — a Fact with a `trigger` and `done_at`.
- `Cue` — an entity or tag; `Edge` — `derives | supersedes | retracts | relates | involves`.

**Operations (8)**
1. `OBSERVE(trace) → id` — append; never rejects on similarity.
2. `RECALL(query, as_of?, filters, k) → ranked units` — side effect: emits `ACCESS` events.
3. `INSPECT(id) → unit + provenance + lifecycle history` — includes suppressed.
4. `REVISE(old, new) → edge` — `supersedes`; closes `old.valid_until = now`; `recorded_at` never changes.
5. `SUPPRESS(id, reason)` — `retract | forget | expire`; sets `invalidated_at`; read-side only.
6. `LINK(a, b, rel)` / unlink by `SUPPRESS(edge)`.
7. `CONSOLIDATE(ids[] → fact)` — only op that creates derived units; writes `derives` edges; marks inputs `merged` (still inspectable).
8. `FEEDBACK(id, signal ∈ {used, ignored, wrong, pin})` — the adaptivity channel.

**Invariants**
- I1 Append-only: no op deletes a unit or edge.
- I2 Provenance closure: every Fact reaches ≥1 Trace via `derives/sources`.
- I3 Bitemporal: `recorded_at` immutable; validity intervals only close, never reopen.
- I4 Currency is a filter: "current" = `invalidated_at IS NONE AND valid_until > as_of`.
- I5 Strength is a prior, never a gate: no unit becomes unreachable by decay.
- I6 Single deriver: only `CONSOLIDATE` (batch) writes Facts, tags, or merges.
- I7 Bounded growth: active Facts ≤ Traces; chunks and events carry no lifecycle.

**Lifecycle states**: `active → superseded | retracted | expired | merged` (all recoverable via `INSPECT`); `pinned` exempts from TTL and strength decay.

**Smriti mapping**: `kb_remember→OBSERVE` (type/subtype become hints); `kb_recall + kb_recall_at→RECALL`; `kb_drill→INSPECT`; `kb_archive, kb_relation_retract→SUPPRESS`; `kb_link→LINK`; `kb_consolidate, kb_consolidation_candidates→CONSOLIDATE`; `kb_promote→FEEDBACK(pin)`; `kb_queue→OBSERVE(intent)`; `kb_entity_*` = Cue ops (keep). `kb_agent_*`, `kb_status/alerts/export/import/reembed/regenerate_md` are admin, outside the protocol. 23 tools → 8 protocol + 3 cue + admin.

## 8. Questions for other teams

1. **Retrieval**: with `bench/` restored and n large enough to clear the ±7 pt floor, does a strength prior fused after rerank move Recall@10, or only Recall@1?
2. **Storage**: SurrealKV HNSW — does it accept int8 vectors, and what is the per-index write cost that justifies cutting from 10 to 5 indexes?
3. **Voyage**: voyage-context-4 max chunks per document and whether a trace's `summary` can be the "document context" for a single-chunk memory (would unify the write path).
4. **LLM/judges**: Jev tag-classifier accuracy against the current 23 subtypes on existing rows — is the taxonomy recoverable as tags with >90% agreement, or was it never predictive?
5. **Owner**: is caller classification a preference you want kept, or a burden you accepted? The protocol makes it optional either way.
6. **Security**: `access_event` stores ids and ranks only — confirm this satisfies "never log memory content."

## Team storage
# Storage & Systems Team: Minimal Durable Memory Store

Repo root: `/Users/dhruv/projects/system/smriti/.claude/worktrees/llm-runtime-quality-adr`

## 1. Diagnosis (storage lens)

1. **There is no immutable truth layer.** The `memory` row holds both the truth and every derived field: summary, vector, SM-2 state, importance and status. Prune lowers `importance` in place (`src/db/prune.rs:80-147`), which overwrites the caller's original rating. That is quiet information loss.
2. **Reads write to truth rows.** `src/db/memory.rs:517,528,556` runs `access_count += 1, updated_at = time::now()` on recall and drill. This costs a write on every read, and it corrupts transaction time: `updated_at` stops meaning "content changed".
3. **Chunks are full memories.** Content of 2000 chars or more (`src/db/chunking.rs:26`) becomes N sibling `memory` rows. Each has the whole schema, its own vector, about 12 B-tree indexes, 3 BM25 indexes, SM-2 state and TTL logic. Space grows with N times the per-row overhead.
4. **Vectors are stored as `array<float>`** (`src/db/migrations.rs:216`). SurrealDB floats are f64 with per-element encoding, so I estimate about 8-10 KB for each 1024-dim row. Search is a brute-force SurrealQL `vector::similarity::cosine` scan (`src/db/search.rs:1075-1089`); the code comment says it is "sufficient for <100k". At 10^6 rows each query would read and deserialize about 10 GB. It will not scale, and this is the real ceiling before any taxonomy question.
5. **There are seven edge representations**: `involves`, `assigned`, `updates`, `extends`, `derives`, `relates`, `retracts`, plus `typed_relation` and `entity_alias`. A separate `person` table duplicates `entity` with kind=person. Auto-link `relates` edges can be recomputed from vectors but are stored as truth.
6. **Nothing accounts for bytes.** The 281 MB figure is one number with no breakdown into chunks, vectors, indexes and payload.
7. **Growth is unbounded by design.** Forgetting is only a read filter, and every archived row stays in the hot indexes and in the vector scan. Recall cost grows with every write ever made, not with what is currently known.

## 2. Target design: one log, rebuildable projections

**L0, the truth log (append-only, never mutated):**
- `obs { id: ULID, hash: blake3(normalized bytes) UNIQUE, recorded_at, event_at?, source{agent, session, ref, channel}, visibility, media_type, payload: zstd bytes | blob_ref }`.
  - The content hash makes writes idempotent: the same input twice returns the same id.
- `event { id, at, actor, op, target, args }` records every revise, suppress, link, feedback and fold.
- The current state of everything is a fold over `obs` plus `event`.

**L1, derived units (what recall returns; rebuildable):**
- `unit { id, obs_ids[], text ≤500, kind?: u8, anchors ≤3, valid_from, valid_until?, recorded_at, state: active|superseded|suppressed|folded|expired, superseded_by?, folded_into? }`.
  - `kind` is an optional hint, not something the caller must classify. That question belongs to the taxonomy team.
- `span { obs_id, start, end, vec }` replaces chunk-memories. Span text is a byte range into the verbatim obs, so nothing is copied.
- `link { from, to, rel, confidence, origin: caller|extractor|auto, obs_id? }` is one table for every edge kind, with additive semantics. Auto-links become a cache that can be dropped and rebuilt.

**L2, the index (disposable; rebuild with `REBUILD`):**
- Voyage `voyage-context-4`, with the whole obs sent as the "document" and its units and spans as the "chunks". Each unit vector then carries its source context, which fixes embed-text starvation (D3) without the content-prefix workaround.
- Two vector codes per unit and span:
  - `bin256`: 256-dim ubinary, 32 B, held in RAM.
  - `i8_1024`: 1024-dim int8, 1 KB, memory-mapped.
- No float copy is kept, because vectors can be rebuilt from the text.
- **Recall pipeline:**
  1. In-process Rust Hamming scan over `bin256`. At 10^6 that is 32 MB, about 5-10 ms. No ANN index is needed below about 10^7.
  2. Take the top 1000 and rescore them on `i8_1024`.
  3. Fuse with BM25 over `unit.text` and `span` text, using RRF.
  4. Keep a pool of 150 (P0.5), then rerank to the top 50 with `rerank-2.5-lite`.
- **Tiers:**
  - Hot: active units, in the vector and BM25 index.
  - Warm: superseded, suppressed and folded units. These have text and provenance but are not in the hot index; `RECALL(depth=all)` scans them.
  - Cold: obs payloads, zstd-compressed, optionally sealed into monthly Parquet or JSONL segments.

**Consolidation compacts the index, not the truth.** A FOLD of N units produces one new unit. The N units go to `folded` and leave the hot index. The obs log and the event log keep everything, and the folded unit stays reachable through `folded_into`. FOLD reuses the `consolidation_attempt` ledger and ADR-026, and it stays inside the judge additive-only invariant because it never archives or retracts truth.

## 3. Delete or simplify

| Item | Action |
|---|---|
| SM-2 fields `ease_factor`, `review_interval`, `next_review_at`, and the prune decay that mutates `importance` | Delete. Salience becomes a derived projection over feedback events. |
| `access_count` and `updated_at` bumps on read | Delete. Use FEEDBACK events instead. |
| Chunk-as-memory rows; content FTS over 500 KB blobs | Replace with `span`. |
| `domain_decay_rate` (dead code) | Delete. |
| `status` hint plus a separate `invalidated_at` B-tree | One `state` u8. Keep the bitemporal fields (ADR-011). |
| 6 legacy edge tables plus `typed_relation` | One `link` table. |
| `person` table | Fold into `entity`. |
| B-tree indexes on type, importance and domain | Drop any index no query plan uses; at 10^6 these filters work after the scan. |
| `kb_recall_at` as a second pipeline | Merge into `RECALL(as_of)`, which ends the D6 drift. |
| TTL for working/* | Becomes a default `valid_until`, so one mechanism covers it. |

## 4. Guarantees

- **No forgetting:**
  - I1: obs and events are append-only.
  - I2: every unit, span, link and vector can be rebuilt from the log plus a recorded model fingerprint (Migration 015, extended with dim and dtype).
  - Every non-active state can still be reached with `depth=all` or `INSPECT`.
- **No bloat:**
  - Hot index size is about the number of currently valid, unfolded units, not the number of writes.
  - Supersede, fold and suppress shrink the hot set.
  - A configurable hot budget demotes units with the lowest salience to warm. Demotion can be undone.
  - The log grows linearly with real input, which is the minimum possible if nothing is to be lost, and compresses to about 0.4×.
- **Adaptivity:**
  - FEEDBACK events (`used`, `wrong`, `irrelevant`, `pin`) and recall co-occurrence feed a derived `salience` table with time decay.
  - Salience is used as a fusion and rerank feature and as the demotion order.
  - Because it is derived, the salience policy can be changed later and replayed over history.

## 5. Cost and space at 10^6 units (assumed: 1.5 KB raw per obs)

| Layer | Target | Current layout (estimate) |
|---|---|---|
| obs log (zstd) | ~0.6 GB | content duplicated in chunk rows |
| unit and span rows | ~0.4 GB | ~3-5 GB with indexes |
| vectors | 32 MB RAM + 1 GB mmap | ~8-10 GB as f64 arrays |
| BM25 | ~0.4 GB | 3 indexes, including the content blob |
| **Total** | **~2.5 GB (~2.5 KB per memory)** | **~15-25 GB** |

- Query latency would be about 10 ms for the scan plus about 300 ms for the rerank round trip; today's p50 is 1167 ms.
- Rerank cost is about 20 tokens × 50 + 50 × 300 ≈ 16K tokens, roughly $0.0003 per query.
- A full re-embed is about 1.5e8 tokens, inside the 200M free tier. At list price it would be about $18, or about $12 through the Batch API.
- The main risk is first-pass recall loss from binary codes. The gate is that Recall@150 must match float on LoCoMo before the float copy is dropped.

## 6. Staged migration (builds on P0)

- **S0 (this branch):** add byte accounting to `kb_status` diagnostics (rows by kind and state, chunk rows, vector, index and payload bytes). Restore the bench. Stop reads from writing `updated_at`.
- **S1 (Migration 022):** add the `obs` and `event` tables with hash idempotency. The write path writes the obs first, and the existing `memory` row becomes the unit. Backfill obs from current summary and content.
- **S2:** convert chunk rows into `span` rows on the existing `voyage-context` path in `chunking.rs`, and soft-archive the old chunk rows.
- **S3:** add byte columns via Voyage `output_dtype` and the Hamming scan, with `kb_reembed` through the Batch API. A/B against float, then drop the float column.
- **S4:** unify links and move salience to feedback events. Delete SM-2 decay.
- **S5:** hot/warm tiering and FOLD-on-consolidate.

ADR-011, the judge invariant and the Sensitive read-filter decision stay unchanged throughout.

## 7. The fundamental memory protocol (one page)

**Data units**
- **Observation:** verbatim, immutable and content-addressed. It is the only truth.
- **Memory:** a claim derived from one or more observations, with `[valid_from, valid_until)`, `recorded_at` and provenance.
- **Link:** a typed, directed, additive relation between memories or entities, with confidence and origin.
- **Event:** every state change, which is itself logged.

**Operations**
1. `OBSERVE(payload, source, event_time?, visibility, hints?) → obs_id, memory_ids`. It is idempotent and never rejected for lack of classification.
2. `RECALL(query, as_of?=now, filters?, k, depth=hot|all) → [memory + provenance + validity], recall_id`. It is a pure read.
3. `REVISE(memory_id, payload | valid_until, reason ∈ {update, correct, expire})`. This writes a new obs and memory and closes the old memory's validity window. It never deletes.
4. `LINK(a, b, rel, confidence, origin)` and `UNLINK`. UNLINK closes the link's validity window.
5. `FEEDBACK(recall_id?, memory_id, signal ∈ {used, wrong, irrelevant, pin})`. This is the only adaptivity input.
6. `SUPPRESS(target, reason)` and `UNSUPPRESS`. Forgetting means removal from default retrieval, and it can be undone.
7. `INSPECT(id) → full lineage`: obs, events, versions and links.

System-owned operations:
- `FOLD(memory_ids) → memory`: compacts the index only.
- `REBUILD(projection)`: re-derives from the log after a model change.

**Invariants**
- I1: truth is append-only.
- I2: every derived artifact can be rebuilt and traces to at least one obs.
- I3: bitemporal. `RECALL(as_of=t)` is reproducible.
- I4: reads never mutate truth.
- I5: the hot set stays under budget, and overflow is demoted, never lost.
- I6: writes are idempotent by content hash.

**Lifecycle**
- `pending → active → {superseded | suppressed | folded | expired}`.
- Every state can be reached with `depth=all`.
- The only exit from the log is an out-of-protocol `PURGE` admin operation for legal erasure, which leaves a tombstone event.

**Smriti mapping (about 25 tools become 7 operations plus admin)**

| Smriti tool | Protocol operation |
|---|---|
| `kb_remember` | OBSERVE; type and subtype become optional hints |
| `kb_queue` | OBSERVE with channel=queue |
| `kb_recall` + `kb_recall_at` | RECALL |
| `kb_drill` | INSPECT |
| `kb_archive(retract)` and `updates_id` | REVISE |
| `kb_archive(forget)` and `kb_entity_delete` | SUPPRESS |
| `kb_link` and `kb_relation_retract` | LINK / UNLINK |
| `kb_promote` | FEEDBACK(pin) |
| `kb_consolidate` and `kb_consolidation_candidates` | FOLD |
| `kb_reembed` and `kb_regenerate_md` | REBUILD |
| `kb_export` | dump the log |
| `kb_import` | replay OBSERVE |
| `kb_entity_*` | a derived view over memories and aliases |
| `kb_agent_*` | the source/identity layer, outside the memory protocol |
| `kb_status` and `kb_alerts` | admin |

## 8. Questions for other teams

1. **Taxonomy:** does any `kind` field need to exist at the storage level, or only as a rerank and salience feature? Which floors survive?
2. **Retrieval:** what is the acceptable Recall@150 loss for a binary first pass with int8 rescoring? Is 1000 enough oversampling?
3. **Voyage:** does `voyage-context-4` support `output_dtype` and `output_dimension`? Is the voyage-4 family one shared embedding space, which would allow lite for queries and large for documents? Both are unverified.
4. **LLM/extraction:** who derives memories from an obs (the caller's summary or Jev), and how often should we re-derive when models change?
5. **Privacy/owner:** is legal-erasure `PURGE` required? It is the only allowed exception to I1.
6. **SurrealDB:** can byte columns be bulk-loaded into Rust fast enough, or should vectors live in a sidecar mmap file that is a pure derived projection?

Key files:
- `/Users/dhruv/projects/system/smriti/.claude/worktrees/llm-runtime-quality-adr/src/db/migrations.rs`
- `/Users/dhruv/projects/system/smriti/.claude/worktrees/llm-runtime-quality-adr/src/db/memory.rs`
- `/Users/dhruv/projects/system/smriti/.claude/worktrees/llm-runtime-quality-adr/src/db/chunking.rs`
- `/Users/dhruv/projects/system/smriti/.claude/worktrees/llm-runtime-quality-adr/src/db/search.rs`
- `/Users/dhruv/projects/system/smriti/.claude/worktrees/llm-runtime-quality-adr/src/db/prune.rs`

## Team retrieval
# Retrieval and LLM-Runtime Team: Fundamental Memory Protocol and Smriti Target Design

## 1. Diagnosis (retrieval lens)

These findings add to D1–D10. They do not repeat them.

- **R-a. The unit of storage is the wrong unit of retrieval.** A 10–500 char *summary* is the only text embedded, and it is also the only text returned. The verbatim evidence the answerer needs sits in `content` and comes back only on `kb_drill`. Conditional accuracy is 76.5%, so answer-side loss is real even when retrieval hits. Notes are good *keys* but poor *evidence*.
- **R-b. Chunks are full memories** (`chunking.rs:341`). Every passage carries the full schema, bitemporal fields, SM-2 fields and indexes. Passages compete with their own parent in fusion. They also inherit the parent's subtype, TTL and importance, which are meaningless for a passage.
- **R-c. Vector search reads whole rows.** It runs `SELECT *, vector::similarity::cosine(...)` (`search.rs:1089`) as a full scan with no ANN (MTREE is unsupported on SurrealKV), and it deserialises every row, `content` included, on every query. Vectors are stored as `array<float>`, which is probably f64 plus encoding, so around 8–9 KB per row instead of 4 KB. This needs verifying. The scan is likely a large share of the 853 ms p50 measured before rerank.
- **R-d. Context is faked with a string prefix.** The `[type/subtype][project][domains]` prefix and the P0.8 content prefix imitate what `voyage-context-3` does natively. Smriti already pays for that model (`config.rs:433`) but uses it in single-chunk mode on the main write path, which gives no context at all.
- **R-e. Nothing records what was retrieved or used.** `access_count` is bumped only on drill (`search.rs:1589`). SM-2 "review" is therefore almost never observed, so decay acts on absence of drills, not on absence of use. The "adaptive" goal has no input signal.
- **R-f. Classification is a filter the caller must get right.** A wrong subtype passed as `type=` hard-excludes gold. Nothing measures whether type predicts relevance.
- **R-g. Possible recall hole.** Without `project`, the vector stage keeps only `visibility='standard'` (`search.rs:1078-1081`), so Private memories are never vector-searched cross-project. Is this intended?

## 2. Target design

**Tables**

- **`record`** (new): the immutable source. Fields: `id = blake3(source_ref‖bytes)`, `media`, `text` (zstd once cold), `source_ref`, `recorded_at`, `author`, `visibility`, `sealed_at?`. It holds no vector and has only 2 indexes.
- **`passage`** (replaces chunk-memory rows): `record`, `ord`, `start`, `end`, `vec_i8: bytes(1024)`, `vec_b: bytes(128)`. Its text is a slice of `record.text` and is not stored twice. It has no lifecycle fields.
- **`memory`** (the note, kept): the atomic assertion, with `summary`, `kind`, `event_at`, `valid_from`/`valid_until`, `invalidated_at`, `derived_from: [record|passage|memory]` (required, non-empty), `vec_i8`, `vec_b`, `utility`, `pinned`.
- **`trace`** (new, adaptivity): `request_id`, `query_vec_i8`, `as_of`, `[(id, stage, rank, score)]`, `used: [id]`, `ts`.

**Write (OBSERVE)**

1. Hash the input, and stop if the hash already exists.
2. Store the `record`.
3. Split into sentence-boundary passages of about 300 tokens, no overlap. Contextualized embeddings make overlap unnecessary.
4. Notes come from the caller's hints and/or async Haiku extraction, reusing the P2.2 W4 sidecar.
5. Make **one** `/contextualizedembeddings` call per record, with chunks = [metadata header + note₁…noteₖ, passage₁…passageₙ]. Every note vector is then conditioned on its source. This replaces both the string prefix and P0.8. Records over 32K tokens are windowed.
6. Quantize locally from float to int8 (per-dimension scale saved in `_kb_metadata`) and to sign bits.

**Recall**

1. Run the query embed in parallel with R1 and a rule-based date parse.
2. BM25 over notes and passages.
3. Vector stage: an **in-process SIMD scan** of a derived, rebuildable matrix loaded at startup. It runs Hamming top-1000 on `vec_b`, then int8 cosine top-150. The DB stays the source of truth and the matrix is a cache, not a store.
4. RRF at pool=150 (P0.5), with spreading activation competing in the pool (P0.6).
5. **Collapse passages onto their notes**, keeping the best evidence span per note.
6. Rerank the top 30 using Jev, with Voyage `rerank-2.5` as fallback (P1.2). Each rerank document is `"[event_at ISO] [valid?] note — evidence: passage≤400ch"`. The query carries "today = X" so temporal reasoning is possible.
7. Return each note with its evidence span, absolute dates, `trace_id` and `evidence_sufficiency`.

**Models**

| Use | Choice |
|---|---|
| Documents | `voyage-context-3` at 1024d today. Run `voyage-context-4` ($0.12 vs $0.18/M, free tier) as a P0 bench candidate using the fingerprint gate and the Batch API for re-embedding (33% off). |
| Queries | The same model with `input_type=query`. A mixed-size, shared-space voyage-4 pairing is **unverified** and not adopted. |
| Dimensions | 1024. Do not use 512 or 256 until the bench says so. |
| Storage dtype | int8 plus binary. Float is discarded after quantization. |

## 3. Delete or simplify

| Delete or simplify | Replacement |
|---|---|
| Chunk sibling memory rows | `passage` |
| The 23-subtype caller classification | `kind ∈ {event, fact, procedure, intent}`, optional and server-inferred (Jev Choice, as R1 already plans), plus a `horizon` field that replaces the 5 working subtypes. Old subtypes are kept as tags through a deterministic mapping. Type becomes a **soft boost, never a hard filter**. |
| Importance floors/ceilings and SM-2 fields (`ease_factor`, `review_interval`, `next_review_at`) | Learned `utility` plus `pinned` (`is_static`) |
| `domain_decay_rate` | Nothing; it is dead code |
| MMR, unless P1.5 passes | Nothing |
| `recall_at` as its own pipeline | `recall(as_of=)` |
| The embed-text prefix | The metadata header inside the contextual chunk |
| Dense cosine `relates` auto-links (8 per memory, unmeasured) | Co-retrieval edges mined from `trace` (P3), unless the bench shows `relates` helps spreading |

## 4. Guarantees

- **Nothing is forgotten.**
  - Records are immutable and content-addressed.
  - Every note's `derived_from` resolves to a record.
  - All changes are appends: supersede and suppress set timestamps and never mutate text.
  - Vectors, notes, edges and summaries can all be regenerated from records plus the op log, so they are disposable caches.
- **No bloat.**
  - *Hot set:* the scan matrix holds only active, non-demoted notes and passages. Its size depends on live knowledge, not on history.
  - *Consolidation:* this is the bound-enforcing operation. Sources are demoted out of the hot set, stay reachable through `derived_from` and `as_of`, and their vectors fall to `vec_b` only.
  - *Cold records:* zstd-compressed.
- **Adaptivity.**
  - *Usage signal:* `utility = Beta(used+1, shown−used+1)`, with a 90-day half-life. "Used" is an explicit FEEDBACK, a GET within the session, or a later OBSERVE whose hints cite the id.
  - *What utility drives:* a fusion prior (weight fitted on the dev split, gated), demotion (never used, shown at least N times, older than 180 days), consolidation candidacy, and offline learning-to-rank fitting of `w`/RRF k from traces.
  - *Not driven by utility:* deletion. Utility never deletes anything.

## 5. Cost and space (estimates)

**Per-unit vector size**

| Unit | Today | Target |
|---|---|---|
| Note vector | ~4–9 KB | 1,152 B |
| Passage | full row, ~10 KB+ | ~1.2 KB |

**At 10⁶ notes plus 2×10⁶ passages**

- *Vectors:* about 3.5 GB in total. The hot scan holds about 0.4 GB of binary plus int8 rescoring for the candidates.
- *Scan time:* Hamming over 3M × 128 B is about 10–20 ms on M-series hardware, which is estimated, not measured.

**Spend**

| Item | Volume | Cost |
|---|---|---|
| Embedding writes | ~1k/day × ~600 tok | ≈ $0.07–0.11/day |
| Voyage rerank fallback (lite) | 30 docs × ~450 tok ≈ 14K tok/query | ≈ $0.0003/query |
| Full re-embed of 10⁸ tokens | Batch API | ≈ $8–12 |

## 6. Staged migration (on top of P0–P3)

| Stage | Work |
|---|---|
| **S0** (alongside P0) | Add the `trace` table (IDs plus int8 query vector, no text in logs) and the stage-wise gold tracking from P0.1. Measure the byte breakdown of the 281 MB. Add `context-4` as a P0 bench arm. |
| **S1** | Migration adds `record` and `passage`. Existing chunk rows convert to passages (old IDs kept as aliases) and their parent's `content` becomes a record. Add the in-process scan with int8/binary. Merge `recall_at` into `recall` (P1.4 → delete). |
| **S2** | Contextual write embedding as one call per record. Return evidence spans. Add `kind`/`horizon`, with the subtype becoming an optional tag. Note extraction reuses W4 and Haiku. |
| **S3** | `utility` drives the prior, demotion and consolidation (with P3 reconsolidation fed by `trace`). Retire SM-2. |

Each stage is gated on the full LoCoMo10, as in §5 of the proposal, plus LongMemEval (knowledge-update and temporal categories).

## 7. Fundamental Memory Protocol (MP/1)

**Data units**

- **Record**: immutable source bytes, content-addressed, with `recorded_at`, `author`, `visibility`.
- **Note**: one atomic assertion of 500 chars or less. It has `kind`, `event_at`, a world-validity interval `[valid_from, valid_until)`, `derived_from` (required) and `utility`.
- **Entity**: a stable identity with aliases.
- **Link**: a directed, typed, timestamped edge with provenance.
- **Trace**: one retrieval event plus the feedback on it.

**Operations** (8; all writes are idempotent)

| Op | Semantics |
|---|---|
| `OBSERVE(content, source, time?, hints?)` | Adds a Record and zero or more Notes. A repeated input returns the existing ids. |
| `RECALL(query, as_of=now, filters?, k)` | Returns Notes with evidence spans and a `trace_id`. It is deterministic in system time. |
| `GET(id, depth?)` | Returns a Note, Record or Entity with its provenance and history. |
| `REVISE(id, text?, reason∈{update, correct, expire}, valid_until?)` | Appends a successor Note and closes the old one. It never edits. |
| `LINK(from, rel, to, provenance)` | Adds or closes an edge. |
| `CONSOLIDATE(ids)` | Produces a summary Note derived from those ids and demotes the sources. |
| `SUPPRESS(id, scope, reason)` / `UNSUPPRESS` | Hides an item from RECALL. It is reversible and nothing is destroyed. |
| `FEEDBACK(trace_id, used[], unhelpful[], missing?)` | The only input that adapts the system. |

`PURGE(record)` exists only as an admin-console operation for legal or privacy needs. It is not on the agent surface and it is logged.

**Invariants**

1. **Provenance closure.** Every Note reaches a Record through `derived_from`.
2. **Append-only.** History is never lost, and every derived artifact can be rebuilt from Records plus the op log.
3. **Bitemporal.** "Current" is a predicate over world time and system time, never a deletion.
4. **Bounded hot set.** The hot set is at most the active set, and CONSOLIDATE strictly shrinks it.
5. **Additive automation.** Automated agents may only OBSERVE, LINK or tag. REVISE and SUPPRESS need an owner or the author.
6. **Read-side visibility.**

**Lifecycle states**

- Note: `active → superseded | expired | demoted | suppressed`. All are recoverable through GET and `as_of`.
- Record: `live → sealed`.

**Mapping Smriti's 25 tools to MP/1**

| Smriti tools | MP/1 operation |
|---|---|
| `kb_remember`, `kb_queue` | OBSERVE |
| `kb_recall`, `kb_recall_at` | RECALL |
| `kb_drill`, `kb_entity_get`, `kb_entity_find` | GET, or RECALL scoped to entities |
| `kb_remember(updates_id)`, `kb_archive(retract)` | REVISE |
| `kb_archive(forget)`, `kb_entity_delete` | SUPPRESS |
| `kb_link`, `kb_relation_retract`, `kb_entity_upsert` | LINK, or OBSERVE for an entity |
| `kb_consolidate` | CONSOLIDATE |
| `kb_promote` | FEEDBACK(pin) |

That leaves **8 agent tools**. Everything else moves to admin: status, profile, alerts, export, import, regenerate_md, consolidation_candidates, reembed and agent register/retire/query.

## 8. Questions for other teams

1. **Storage.** What is the actual byte split of the 281 MB, and the row count? Does SurrealKV store `array<float>` as f64? Is the `bytes` type compact?
2. **Taxonomy and lifecycle.** Do the 4 kinds plus `horizon` cover every working-memory TTL use case? Is PURGE acceptable under the owner's rule that nothing is forgotten?
3. **LLM.** Should note extraction use Jev span-Choice, or Haiku generation? What is its fidelity gate?
4. **Eval.** Can LongMemEval be added to `bench/`? Is `trace` replay accepted as an offline learning-to-rank dataset?
5. **Owner.** Is the Private-visibility vector exclusion when no project is given intentional (`search.rs:1078`)?
6. **Vendor.** Can someone verify the `voyage-context-4` dimension/dtype support, the 32K-per-document limit, and the voyage-4 shared embedding-space claim from Voyage's own pages?

## Team datamodel
# Smriti Taxonomy & Data Model — Independent Review

## 1. Diagnosis

Smriti's bloat isn't row count — it's **structural weight per row**. Every memory, regardless of how small or short-lived, carries the full schema: 4 bitemporal timestamps, importance with per-subtype floor/ceiling enforcement, visibility, SM-2 fields, 10 B-tree/BM25 indexes, and (for ≥2000-char content) a vector. The 23-subtype taxonomy doesn't just classify — it *gates*: it decides TTL, decay immunity, importance bounds, and `is_static`, all from a single caller-asserted enum pair that the server trusts at write time and never re-evaluates.

Three concrete symptoms:

- **Chunking multiplies the heaviest row in the schema.** `chunking.rs` builds each chunk from a full `ChunkParent` (importance, visibility, tags, channel, modality, expires_at, author) — a 20KB transcript becomes 10+ rows that are each independently indexed, decayed, and TTL'd, none of which chunk text actually needs.
- **Taxonomy has no feedback loop.** Floors/ceilings (decision≥4, value=5…) are static, hand-written, and never checked against whether the subtype predicts retrieval value (STATUS D10). A caller miss-classifying `fact` vs `pattern` has permanent consequences (importance ceiling, decay eligibility) that nothing ever revisits.
- **Dead config is a symptom, not an accident.** `domain_decay_rate` is computed and never called. Config surface (floors, visibility rules, dedup thresholds, decay rates) is growing faster than anything validates it earns its keep — same blind spot as the retrieval team's unmeasured heuristics, one layer down.

The 5×23 split optimizes for a taxonomy nobody asked for over the phone test: no reference system we were given (KG server, Supermemory, Mem0, Graphiti, Letta) asks the caller to pick from 23 options at write time. They all get structure from **time + provenance + a server-side pass**, not caller classification.

## 2. Target design

Three layers, not one flat `memory` table wearing every hat:

**Raw layer (new, thin).** `raw_source {id, bytes_ref|text, mime, ingested_at, source_ref}`. Immutable, append-only, never decayed or TTL'd. This is what Supermemory calls "document" and what the KG reference lacks entirely — without it, re-extraction when models improve is impossible. Smriti already has `source_ref`; it just doesn't point at anything durable yet.

**Observation layer (the current `memory` table, slimmed).** One atomic claim per row:
```
observation {
  id, text (summary, 10-500c), content_ref -> raw_source (optional),
  kind: Durable | Transient | Task,          -- was: 5 types × 23 subtypes
  event_at, valid_until, created_at, invalidated_at, invalidation_reason,  -- keep, ADR-011 is right
  importance: derived (see §4), visibility, author, project,
  domain: tags[], people: tags[],
  vector, anchor_entities
}
```
`kind` replaces `memory_type` and drives only the three things that actually need a closed enum: TTL eligibility (`Transient`), task semantics (`Task` gets an `assigned` edge + due-date), and decay eligibility. Everything the current 23 subtypes encode — decision vs fact vs preference, meeting vs session, goal vs life_goal — becomes a **tag**, written by the caller if they want to, corrected by a cheap server-side classifier (Jev, already in the P0-P3 roadmap) if they don't, and *revisable without touching the row's identity*. This is the KG reference's `entityType`-as-string instinct, generalized: a memory's genre is metadata, not its spine.

**Chunk layer (new, lightweight).** `chunk {id, parent_id -> observation, text, vector}`. No importance, no visibility, no bitemporal fields, no SM-2 — it inherits all of that from `parent_id` by join. This is the single biggest space win available (§5).

**Entities/relations** stay as-is — the KG reference's `{name, entityType, observations[]}` model is already what Smriti's entity table + `involves`/`typed_relation` edges implement, and ADR-030's alias table is a genuine improvement over the reference server's flat name key. Don't touch it.

## 3. Delete or simplify

- **Delete the 23-subtype importance floor/ceiling table.** Replace with importance as a derived score (§4) plus one caller-settable boolean `pin: bool` for the decision/value/life_goal "never decay" cases — 1 bool beats 3 hardcoded immune subtypes.
- **Delete `domain_decay_rate`** (dead code, STATUS D9/D10) — no bench ever exercised it.
- **Collapse `working/{priority,open_question,scratch,signal,hypothesis}`** into `kind=Transient` + a caller- or policy-supplied `ttl: Duration`. Five enum variants that each require a code change to retune become one field.
- **Make `memory_type`/`memory_subtype` write-optional**, inferred server-side when omitted. Keep the columns (don't drop data) but stop requiring the caller to get them right.
- **Chunk rows lose their own importance/visibility/bitemporal/SM-2 fields** — inherited, not duplicated.
- **Keep**: bitemporal fields (ADR-011's reasoning against edge-timestamps is correct and shouldn't be redebated), `entity_alias` table, `typed_relation`, `status` as a derived hint, judge additive-only invariant, visibility-doesn't-gate-enrichment decision.

## 4. No-forgetting, no-bloat, adaptivity

**No-forgetting**: raw layer is append-only and never touched by decay/archive/TTL — the one thing every reference system (Supermemory, Graphiti, Letta) insists on and the current design lacks. Observations keep soft-invalidation exactly as now (`BITEMPORAL_ACTIVE`).

**No-bloat**: bounded two ways, neither currently present. (a) Chunk rows shed ~8 of 12 schema fields and their indexes. (b) A periodic **compaction** pass (Letta's "sleep-time," absent today) rolls expired `Transient` rows and fully-decayed `Durable` rows with no access in N days into a single `derives`-linked rollup, archiving the originals — archived rows are never purged, but the *active-set* that indexes and recall scan stays bounded. Nothing in the current codebase does this; it's a gap, not a redesign.

**Adaptivity**: importance becomes `f(kind_prior, access_count, recency, explicit pin)` recomputed on a schedule, not asserted once at write time and enforced by a lookup table. Tags inferred wrong get corrected by re-tagging, not by re-deciding the row's floor/ceiling. This is the feedback loop D10 flags as missing — the taxonomy question collapses to "what tag policy predicts retrieval value," answerable from `access_count` data Smriti already logs.

## 5. Cost/space estimate

Per Voyage sizing: 1024-dim float32 = 4KB/vector regardless of row shape — vector cost is fixed either way. The win is in the ~500B–1KB of structural overhead (bitemporal fields, SM-2, importance, visibility, 10 indexes) that a full chunk row pays and a lightweight chunk doesn't. At STATUS's 281MB with unmeasured chunk-row share, a single `COUNT(*) WHERE id IN (SELECT out FROM extends WHERE ...chunk...)` query — not in this review's scope, flagged for the owning team — would convert this from an estimate to a number before migrating.

## 6. Staged migration (no data loss)

0. Reuse as-is: ADR-011 bitemporal, ADR-030 aliases, ADR-023 spreading activation, current P0 retrieval fixes.
1. Add `kind` (3-valued), backfill deterministically from existing type/subtype; old columns stay, now deprecated-but-present.
2. Add `tags`-based subtype migration: one script copies each subtype string into `tags`; `kb_remember` makes type/subtype optional with server-inferred default.
3. Add `chunk` table; migrate existing chunk-shaped `extends` memory rows by copying text+vector, archive (not delete) originals, dual-read during transition.
4. Flip importance to derived behind a flag; delete `domain_decay_rate`.
5. Only after a confidence window, drop the now-unused subtype-enforcement code path (never the data).

## 7. The fundamental protocol (one page)

**Atomic unit**: the *Observation* — `(text, embedding, provenance, event_at, valid_until)`. Everything else is metadata on it or an edge between two of it.

**Operations** (6, exact semantics):
- `Write(text, provenance, event_at?) -> id` — always creates; never overwrites.
- `Recall(query, filters) -> [id]` — read-only, time-scoped by default to "now."
- `Supersede(old_id, new_id)` — sets `old.valid_until = new.event_at`; both remain readable at-time.
- `Link(a, b, relation)` — typed, directed, additive-only.
- `Consolidate([ids], summary_id)` — archives sources, writes `derives` edges; sources never deleted.
- `Suppress(id, reason)` — sets `invalidated_at`; removes from default recall, never from storage.

**Guarantees**: nothing is ever hard-deleted (bounded growth is achieved by *archival compaction*, not erasure); every fact carries provenance to a raw source; truth is bitemporal (`event_at`/`valid_until` vs `created_at`/`invalidated_at`); adaptivity is a scoring function over usage signals, never a write-time enum choice.

**Smriti mapping**: `kb_remember`→`Write`, `kb_recall`/`kb_recall_at`→`Recall`, `kb_archive(updates_id)`→`Supersede`, `kb_link`→`Link`, `kb_consolidate`→`Consolidate`, `kb_archive(forget/retract)`→`Suppress`. Cut: `kb_promote` (folds into derived importance), separate type/subtype validation surface (folds into tags + `kind`).

## 8. Questions for other teams

1. **Retrieval**: does a lightweight `chunk` table (no vector-adjacent metadata) break spreading activation's per-edge-type cap or the sidecar's candidate-ID resolution (D5)?
2. **Ops**: can a full `kb_reembed` migration (Stage 3) ride the same Voyage Batch API window already approved for the P0.8 content-prefix reembed, or does it need its own?
3. **Owner**: is changing `kb_remember`'s required `type`/`subtype` params to optional an acceptable breaking change to the MCP tool contract, given external callers may already depend on it?

# Cross-examination

**Cross-examination: Smriti memory protocol**

Code checks done (worktree, read-only). Confirmed: brute-force `SELECT *` cosine scan with no ANN (`search.rs:1074-1092`, `migrations.rs:212-214`); vector stored as `option<array<float>>` (`migrations.rs:216`); drill writes `access_count += 1, updated_at = time::now()` (`memory.rs:517,528,556`), recall does not (`search.rs:1589`); `domain_decay_rate` has zero callers; `person` table exists beside `entity` (`migrations.rs:183-186`); sidecar `index.ts` contains no "candidate" — D5 stands; contextualized endpoint used only in `chunking.rs:279-300`, main write path single-chunk; dedup thresholds in `dedup.rs:10-11`. Not verified: f64 on-disk width of `array<float>` (plausible, measure in S0); any voyage-4 "shared embedding space across sizes" claim (absent from Voyage brief — treat as false until shown).

---

## 1. Q&A log

**Cognition→Retrieval Q1 (strength prior: Recall@10 or @1?).** Retrieval answers: unknown; `bench/` is missing (D8). Ruling: no prior ships until the bench runs at n large enough to clear ±7pt. Retrieval's `trace` table is the prerequisite for ever answering this.

**Cognition→Storage Q2 (int8 in SurrealKV HNSW; index write cost).** Storage answers: no SurrealKV ANN exists at all (confirmed `migrations.rs:213`), so the question is moot — vectors go to an in-process rebuildable matrix (`bytes` columns or sidecar mmap), and the DB vector field becomes a cache. Index count: drop any B-tree no query plan uses; at 10^6 post-scan filters suffice.

**Cognition→Voyage Q3 (context-4 chunk limit; summary as document context).** Nobody can answer from the brief; 32K/doc figure is a LiteLLM secondary source. Ruling: unverified, gate on S0 vendor check. Retrieval's design (one contextualized call per record with header+notes+passages as chunks) is the right shape *if* it holds.

**Cognition→LLM Q4 (is the 23-subtype taxonomy recoverable as tags >90%?).** Datamodel answers: irrelevant to the decision — the subtype moves to `tags` deterministically either way; the Jev classifier is for *new* writes without hints. Measure agreement only as a curiosity.

**Cognition→Owner Q5 / Datamodel→Owner Q3 (keep caller classification? breaking MCP change?).** Owner has not answered. Ruling: make `type/subtype` *optional* (non-breaking: existing callers keep passing them, they land as tags/kind). No one loses anything.

**Cognition→Security Q6 (access_event = ids+ranks only).** Storage/Retrieval concur; Retrieval adds an int8 *query vector* to `trace`. Ruling: ids, ranks, scores allowed; query vector allowed (not reversible to text in practice, but flag for owner); query *text* never logged.

**Storage→Taxonomy Q1 (does `kind` need to exist at storage level; which floors survive?).** Datamodel + Retrieval agree: a 3–4-valued `kind` exists as a column because it drives *mechanics* (TTL eligibility, intent/trigger semantics); genre is tags. Floors: none survive; `pinned` replaces them.

**Storage→Retrieval Q2 (acceptable Recall@150 loss for binary+int8).** Retrieval answers: zero loss on LoCoMo Recall@150 vs float before the float copy is dropped; 1000 oversample is a starting point, bench decides.

**Storage→Voyage Q3 (context-4 `output_dtype`/dim; shared space lite/large).** Brief lists dtype/dim only for voyage-4/3.5/code-3, *not* context models. Ruling: assume context-4 returns float only; quantize locally (Retrieval's plan). Mixed-size pairing rejected.

**Storage→LLM Q4 (who derives memories from an obs; how often re-derive?).** Retrieval answers: caller hints first, async Haiku/Jev extraction second (P2.2 W4). Re-derive only on explicit `REBUILD` after a model fingerprint change — never on a schedule.

**Storage→Owner Q5 / Retrieval→Taxonomy Q2 (is PURGE allowed?).** All three teams independently invented an out-of-protocol, logged, admin-only PURGE. Ruling: allowed, leaves a tombstone event, never on the agent surface. This is the single sanctioned exception to append-only.

**Storage→SurrealDB Q6 (bytes bulk-load fast enough?).** Unmeasured. Ruling: S1 measures; default to sidecar mmap file (pure projection) if load >1s at 10^5.

**Retrieval→Storage Q1 (byte split of 281MB).** Nobody knows. S0 task #1 for all teams.

**Retrieval→Eval Q4 (LongMemEval; trace replay as LTR set).** Yes to both, contingent on `bench/` restoration.

**Retrieval→Owner Q5 (Private excluded from vector stage without project — intentional?).** Confirmed in code. Cognition's I5 ("nothing unreachable by decay") and the owner's "Sensitive is read-side only" decision both imply Private should be *searched* and *filtered by caller identity*, not silently dropped. Ruling: bug, fix in S0.

**Datamodel→Retrieval Q1 (does light `chunk` table break spreading activation or D5?).** Retrieval answers: no — chunks/passages collapse onto their parent note *before* graph stages; only notes participate in edges. D5 is fixed independently by sending candidate IDs.

**Datamodel→Ops Q2 (one Batch API reembed or two?).** One: S3 quantization and contextual re-embed are the same job.

## 2. Contradictions and rulings

| Conflict | Ruling |
|---|---|
| Dedup: Cognition says never reject on traces; current code rejects >0.92 | **Cognition wins for the raw layer** (idempotency by content hash only). Near-duplicate *notes* still warn; merge happens in CONSOLIDATE. |
| Chunk embedding: Datamodel keeps `[type][project]` prefix; Retrieval/Storage/Cognition use contextualized call | **Contextualized wins** — it is what Smriti already pays for, and P0.8's prefix becomes a fallback for the local embedder only. |
| Number of ops: 6 (Datamodel) vs 7 (Storage) vs 8 (Cognition, Retrieval) | **8.** Datamodel's 6 lacks FEEDBACK — without it the "adaptive" goal has no input (R-e). INSPECT is needed because RECALL hides suppressed items by default. |
| Reranker: Retrieval wants Jev primary, Voyage fallback; others say rerank-2.5-lite | **Both are already decided in P1.2**; not re-opened. Cap candidates at 30–50 before rerank (billing scales query×docs). |
| Who may REVISE/SUPPRESS: Retrieval I5 says owner/author only; Cognition lets replay resolve supersession | **Retrieval wins**, it is the existing judge invariant. Replay may *propose* (RaiseAlert/Tag) and may FOLD (which is index compaction, not truth change). |
| Working memory: `ttl` (Cognition/Datamodel) vs `horizon` (Retrieval) vs default `valid_until` (Storage) | **Storage wins**: one mechanism, `valid_until`. No new field. |
| Importance: delete (Storage) vs derived (Datamodel) vs single prior (Cognition) | Keep as a 1–5 **caller prior, immutable**; derived salience is a separate column. Prune mutating `importance` in place (`prune.rs:80-147`) is the information loss Storage flagged. |
| Vector dtype: Cognition says int8 optional; Storage/Retrieval say binary+int8 mandatory | Mandatory only as a *bench-gated* step. Fits Voyage brief ("don't quantize prematurely") — but the brute-force `SELECT *` scan is the actual ceiling, and the in-process matrix is the fix regardless of dtype. |

## 3. Complexity attack — what breaks if deleted?

| Component | Delete it? | Reason |
|---|---|---|
| Raw record/obs layer | **Keep** | Without it, re-extraction on model change is impossible; sole basis of "nothing lost". |
| `event` op log (Storage) | **Keep, minimal** | Needed to replay REBUILD and audit SUPPRESS; ~1 row/write. |
| `span`/`passage`/`chunk` table | **Keep** | Deleting it means chunk-as-memory persists (confirmed `chunking.rs:249`). |
| `trace`/`access_event` | **Keep** | Only feedback channel; without it adaptivity is a slogan. |
| Salience/strength/utility column | **Keep, as fusion prior only** | Delete and `trace` has nothing to fold into. Never a filter. |
| `kind` column | **Keep (4 values)** | Delete and TTL/intent semantics need another column anyway. |
| 23 subtypes as required | **Delete (→ optional tags)** | Nothing breaks: callers still may pass them. |
| Importance floors/ceilings, SM-2 fields, `domain_decay_rate`, `archive_below_threshold` | **Delete** | SM-2 observes drills only (`search.rs:1589`); decays on absence of a signal that is never emitted. |
| Hot/warm tiers | **Keep** | The only thing that bounds the scan set; "no bloat" is otherwise unachievable under append-only. |
| Hot *budget* with demotion by salience | **Defer** | Only needed past ~10^6 active notes; CONSOLIDATE suffices first. |
| ACT-R/Beta formula specifics | **Defer** | Pick one in S3 on the bench; protocol only says "derived from FEEDBACK, decays". |
| `person` table | **Delete (fold into entity)** | Confirmed duplicate. |
| Seven edge tables → one `link` | **Do it** | Nothing distinguishes them but a string. |
| `recall_at` pipeline | **Delete** | `as_of` param. |
| `relates` cosine auto-links (8/memory) | **Demote to rebuildable cache** | Unmeasured value, pure function of vectors. |
| MMR | Already P0: default off; bench decides. |
| `kb_promote` | **Delete** | = FEEDBACK(pin). |
| Entity/alias/typed_relation | **Keep** | Only working schema layer; D5 is a bug, not a design flaw. |

## 4. Unified protocol candidate — MP/1

**Units**
- **Record** — immutable, content-addressed source (`hash`, `payload`, `recorded_at`, `author`, `visibility`). The only truth.
- **Note** — one atomic claim ≤500 chars: `kind ∈ {event, fact, procedure, intent}`, `tags[]`, `event_at`, `[valid_from, valid_until)`, `recorded_at`, `derived_from[]≥1`, `importance` (caller prior, immutable), `salience` (derived), `pinned`, `state`.
- **Span** — byte range of a Record + vector; no lifecycle.
- **Entity** — identity with aliases. **Link** — directed, typed, with confidence+origin, validity window. **Event** — every state change, appended.

**Operations (8 agent-facing; all writes idempotent)**
1. `OBSERVE(payload, source, event_at?, hints?) → record_id, note_ids` — never rejected for similarity or missing classification.
2. `RECALL(query, as_of=now, filters?, k, depth=hot|all) → notes + evidence spans + trace_id` — pure read.
3. `INSPECT(id) → unit + lineage + all states`.
4. `REVISE(note, text|valid_until, reason ∈ {update, correct, expire}) → successor` — closes old window; text never edited.
5. `LINK(a, rel, b, provenance)` / `UNLINK` (closes window).
6. `SUPPRESS(id, reason)` / `UNSUPPRESS` — hides from default RECALL; reversible.
7. `CONSOLIDATE(ids) → note` — writes `derives`, moves sources to `folded`; strictly shrinks the hot set.
8. `FEEDBACK(trace_id, used[], wrong[], pin?)` — the only adaptivity input.

System-only: `FOLD` (automation's CONSOLIDATE), `REBUILD(projection)`, admin `PURGE(record)` (logged tombstone).

**Invariants**
- I1 Append-only truth; the sole exception is logged PURGE.
- I2 Provenance closure: every Note reaches ≥1 Record; every vector/edge/tag is rebuildable from Records + Events + model fingerprint.
- I3 Bitemporal: `recorded_at` immutable; validity windows close, never reopen; `RECALL(as_of)` reproducible.
- I4 Reads never mutate truth.
- I5 Salience is a prior, never a gate; nothing becomes unreachable by decay.
- I6 Hot set ≤ active set; CONSOLIDATE/suppress/supersede shrink it.
- I7 Automation is additive: may OBSERVE, LINK, tag, FOLD; may not REVISE or SUPPRESS.
- I8 Visibility is read-side only (owner decision).

**Lifecycle**: Note `active → superseded | expired | suppressed | folded` (all via INSPECT/`depth=all`); Record `live → sealed`; `pinned` exempts from TTL and salience decay.

**Smriti mapping**: `kb_remember`, `kb_queue` → OBSERVE; `kb_recall`+`kb_recall_at` → RECALL; `kb_drill`, `kb_entity_get/find` → INSPECT/RECALL(entity); `updates_id`, `kb_archive(retract)` → REVISE; `kb_archive(forget)`, `kb_entity_delete` → SUPPRESS; `kb_link`, `kb_relation_retract`, `kb_entity_upsert` → LINK; `kb_consolidate` → CONSOLIDATE; `kb_promote` → FEEDBACK(pin). Admin: status, alerts, export (dump log), import (replay OBSERVE), reembed/regenerate_md (REBUILD), consolidation_candidates, agent_*.

## 5. Surviving components

Record table; Note (= slimmed `memory`); Span table; single `link` table; `event` log; `trace`; `salience` column; `kind`(4)+`tags`; `pinned`; `valid_until` as the one TTL; entity+alias+typed_relation (with candidate-ID fix); in-process rebuildable vector matrix; contextualized embedding one call/record; pool 150 → rerank ≤50; hot/warm tiers; FOLD via ADR-026 consolidator; byte accounting in `kb_status`; restored `bench/` with LongMemEval.

## 6. Rejected

Required 23-subtype classification; importance floors/ceilings; SM-2 fields and in-place importance decay; `domain_decay_rate`; dedup-reject on raw writes; chunk-as-memory rows; `person` table; seven edge tables; `recall_at` pipeline; `kb_promote`; `horizon`/`ttl` as new fields; mixed-size voyage-4 query/doc pairing (unverified); context-4 `output_dtype` assumption (unverified); dropping float vectors before a Recall@150 parity bench; hot-budget demotion before CONSOLIDATE is shown insufficient; prescribing ACT-R vs Beta in the protocol; silent Private exclusion in vector stage (`search.rs:1078-1081`, fix in S0).