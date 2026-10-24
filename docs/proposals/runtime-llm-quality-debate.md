# Council designs

## Team quality-first
# Quality-First Runtime LLM Architecture for Smriti

**Thesis.** LoCoMo has Recall@10 at 50% and Recall@1 at 12.2%. That points to two separate failures: about half the gold evidence never reaches the candidate pool, and when it does arrive it ranks badly. Each needs a different fix. Fix the measurement first, then the non-LLM pool losses, then add LLM judgment, using the cheapest model that can make each decision.

## 0. Prerequisite: measure each stage before using any LLM

- `bench/` is not in this checkout, and the synthetic oracle is broken. Restore it and add **stage-wise gold tracking**: whether gold is present at BM25@N, at vector@N, after fusion, after rerank, and in the final top-k. Without this nobody can say whether a change fixed pool recall or ranking.
- Rebuild the graded-relevance oracle (ADR-021) **offline** with **Opus 5.5** ($4/$20 per MTok) labelling about 500 probes × 30 candidates, roughly $25–40 one time. **Fable 5.1** ($10/$50) only rules on Opus/human disagreements, about 10% of probes. Neither model runs at runtime.
- Add a quality counter next to the latency histograms in `recall_metrics.rs`.

## 1. Components

### A. Pool and pipeline fixes (no LLM, done first)
- `search.rs:949`: replace the hard `fetch_limit = limit.min(50)` with `[search].pool_size`, default 150 for BM25 and for vector.
- `search.rs:1147-1151`: drop the `primary_cap = limit-3` tax. Spreading-activation neighbours join the **judge pool** and have to earn their slots.
- MMR (`:1181-1201`): run it on the expanded pool and truncate afterwards. Today it reorders a list that is already truncated.
- `should_rerank` (`:857-909`): the final `Semantic`-only branch means episodic/procedural/prospective queries of 4–6 words are never reranked. Set policy=`Always` during the bench, then let the data settle whether to keep ADR-022's gate. Recall@1 of 12.2% is the evidence for revisiting it.
- `recall_at` (`:1583-1847`): add the person filter, alias expansion and spreading activation it currently lacks. Temporal LoCoMo questions take this weaker path today.

### B. Storage fidelity (write path, no generation)
- Today the contextual embed text (`remember.rs:100-124`) is `[type/subtype][project][domains] summary`. Content under 2000 bytes is never embedded. Embed `summary + content` truncated to `embedder.max_input_tokens()`, then run `kb_reembed`. This follows "Fidelity Before Structure" (arXiv 2601.00821), which found verbatim storage beats lossy extraction on LoCoMo.
- Templated chunk summaries (`chunking.rs:302-307`) are identical apart from the index. Replace them with per-chunk summaries from Haiku 5.5, produced asynchronously.

### C. Write-time enrichment ("Enricher", Haiku 5.5, async post-create)
- Add this to post-create step 5 next to anchor extraction. It needs a new migration with these fields: `enrich_questions` (3–5 questions this memory answers), `enrich_terms` (aliases, synonyms, canonical entity names) and `event_time_norm`. `event_time_norm` resolves expressions like "yesterday" or "last week" into absolute dates against `event_at`, which matters a lot for LoCoMo temporal questions.
- BM25 predicate (`:1012`, `:1665`): add `OR enrich_text @@ $query`. Add an optional second vector, `enrich_vector`.
- Invariants: the caller's summary stays canonical. Enrichment never feeds dedup, is never returned as memory text, and skips `Sensitive`. Store `enrich_model` per row so a model swap triggers a backfill (`kb_enrich`, run through `smriti-librarian` like `kb_reembed`).
- Haiku is enough here: this is bounded paraphrase and date normalisation, and Jev cannot generate.

### D. Query understanding (Jev, synchronous, in `recall()` before dispatch, about `:926-957`)
One Jev request: state is the query plus the current date, with these questions answered in parallel.

| Question | Type | Use |
|---|---|---|
| `intent` | Choice {single_hop, multi_hop, temporal, open_domain, unanswerable, procedural} | routing |
| `person` | Choice over up to 254 entity names (candidates from `auto_link::extract_candidates(query)` + alias FTS) + "none" | fills `person` when p ≥ 0.7 |
| `time_scope` | Choice over windows the deterministic parser proposes | soft recency boost, not a hard filter |
| `type_hint` | Choice over the 5 types | soft boost |

- Run it **at the same time as** base BM25/vector. If it detects a person, add a person-anchored list into RRF so the base results are never replaced. Net added latency is about 0–200ms.
- If `intent=multi_hop` with p ≥ 0.6, **Haiku 5.5** writes at most 3 sub-queries, which go through RRF. This happens for roughly 25% of queries and adds about 0.5–0.9s.
- No HyDE. It drifts on precise questions.

### E. Relevance Judge (Jev Score, after Voyage)
- Order: fuse into a pool of 150, Voyage rerank to top 60, Jev judge on the top 30, then MMR, then truncate.
- One request carries 30 parallel `Score(5)` questions, each built from the query, the candidate summary and a content snippet of at most 600 chars. Final score is `w·E[level] + (1-w)·voyage_norm`, with `w` fitted on the bench.
- One more question, `answerable` (Noul) over the top-k set, is returned as an additive `evidence_sufficiency` field so the caller can abstain on adversarial or unanswerable items.
- Why Jev: this is classification, not generation. 30 questions run in parallel, cost is tiny and output is free.
- Hard deadline of 600ms. On timeout it falls back to Voyage order, matching ADR-017's soft-fail.

### F. Write-path judgment (Jev, narrow)
- **Dedup band:** only in the 0.82–0.92 cosine band (`dedup.rs`), ask Noul "Is this the same claim restated?" before create, so embed-first/create-last still holds. This cuts false rejects that lose information and causes misses.
- **Relations:** pass the top 8 `auto_link::find_similar` results into extraction as candidates. Today the `to` field cannot resolve (`sidecar/src/index.ts:215-245`), so edges are near zero by construction. Classify each pair with Jev `Choice` over the 10 relation kinds plus none, which replaces the self-reported generative `confidence`.

### G. Consolidation synthesis: Sonnet 5.5
Merging is generative and errors are hard to undo, so the higher tier is worth it at about $0.01 per cluster. Jev `Noul(should_merge)` acts as the cheap pre-gate.

## 2. Rust trait design

```rust
// src/llm/judge/traits.rs, used as Arc<dyn Judge>, alongside Embedder
#[async_trait]
pub trait Judge: Send + Sync + 'static {
    fn fingerprint(&self) -> JudgeFingerprint;          // provider/model/version
    async fn ask(&self, state: &JudgeState, qs: &[Question])
        -> Result<Vec<Answer>, JudgeError>;              // order-aligned with qs
}
pub enum Question { Noul{name, prompt}, Choice{name, prompt, options: BoundedVec<_, 255>},
                    Score{name, prompt, levels: ScoreLevels /*2..=10*/} }
pub enum Answer { Noul{p: f32}, Choice{probs: Vec<f32>, confidence: f32},
                  Score{expected: f32, probs: Vec<f32>, confidence: f32} }

#[async_trait]
pub trait Enricher: Send + Sync + 'static {
    async fn enrich(&self, m: &EnrichInput) -> Result<Enrichment, EnrichError>;
    async fn decompose(&self, q: &str) -> Result<Vec<String>, EnrichError>;
}
```

- Implementations: `JevJudge` (the `typesafe-jev` 0.2 crate, name checked exactly, `with_transport` for tests), `ClaudeJudge` (Haiku 5.5 structured output, so Judge is not tied to one vendor), `NullJudge`, `MockJudge`. `build_judge(cfg)` is the factory, mirroring `build_embedder`.
- Domain wrappers over `Judge`: `QueryAnalyzer`, `RelevanceJudge`, `DedupJudge`, `RelationClassifier`.
- `Reranker` is currently an enum (`reranker/mod.rs:83`). Leave it alone and have the judge compose after it.

## 3. Config and kill switches

```toml
[judge]            provider="jev"  model="typesafe/jev-1.14"  enabled=false
                   max_usd_per_day=1.0  timeout_ms=600  max_consecutive_failures=5
                   send_visibility=["standard"]
[judge.rerank]     mode="off"   # off | shadow | on
[judge.query]      mode="off"   person_threshold=0.7  multi_hop_threshold=0.6
[judge.dedup]      mode="off"
[enrich]           enabled=false  model="claude-haiku-5-5"
[search]           pool_size=150
```

- `SMRITI_JUDGE_DISABLE=1` and `SMRITI_ENRICH_DISABLE=1` are emergency env overrides.
- Each component gets its own circuit breaker (ADR-026 pattern), so a Jev outage cannot trip the sidecar.
- Model version is pinned (1.14). The `-latest` alias is not allowed.
- **Shadow mode** runs the judge and writes to a `judge_decision` ledger without changing results. The ledger holds IDs, question names, probabilities, latency and model only, **never content**.
- `kb_status(include_diagnostics)` reports the judge and enricher fingerprints, breaker state, and spend from `usage()`.

## 4. Cost and latency (estimates)

| Component | Tokens | Cost | Added latency |
|---|---|---|---|
| Relevance judge (30 candidates) | ~8.5k in | $0.00036/query | 150–500ms |
| Query understanding | ~2k in | $0.00008/query | ~0–200ms net (parallel) |
| Decomposition (Haiku 5.5, ~25% of queries) | 300 in / 120 out | $0.00009 | 0.5–0.9s |
| Enrichment (Haiku 5.5) | 800 in / 250 out | $0.0002/write | async, none |
| Dedup band (Jev) | ~400 in | ~$0.00002 | 70–500ms, band only |
| Consolidation (Sonnet 5.5) | 3k in / 400 out | ~$0.01/cluster | async |

That comes to about **$0.50 per 1k queries** and **$0.20 per 1k writes**. Latency is the constraint, not cost: p95 recall rises by about 0.3–0.7s, or about 1.5s on multi-hop.

## 5. Rollout, with a bench gate at each step

Each step is measured over 3 runs on LoCoMo10 plus the repaired synthetic bench, with a held-out split.

1. **Bench repair and stage-wise tracking.** Gate: baseline reproducible within ±1pt.
2. **Pool, MMR order, primary_cap, `recall_at` parity (A).** Gate: Recall@10 up at least 3pts, p95 up no more than 50ms.
3. **Content in the embedding, then reembed (B).** Gate: Recall@10 up, no regression on the synthetic bench.
4. **Relevance judge, shadow then on (E).** Gate: Recall@1 from 12.2% to at least 20%, MRR up, judge p95 under 600ms.
5. **Enrichment plus backfill (C).** Gate: temporal-category accuracy up at least 5pts, gold-in-pool rate up.
6. **Query understanding and decomposition (D).** Gate: multi-hop and temporal gains, single-hop down no more than 1pt.
7. **Dedup band and relation candidates (F).** Gate: dedup false-reject rate on Opus-labelled pairs, count of persisted typed edges.

Any step that misses its gate is turned off in config, with no code revert.

## 6. Risks

- **Jev calibration is unverified.** Every figure so far comes from the vendor or the crate docs. Fit `w` and the thresholds on Smriti's own labels, and keep shadow mode until then.
- **Privacy.** Jev's retention policy is also unverified. Send only `Standard` memories, and make Jev opt-in like the Voyage gate in ADR-016. `Sensitive` content never leaves the machine.
- **Enrichment hallucination.** Generated fields only help recall and never show up as stored facts. Tag them with `enrich_model` provenance so they can be regenerated.
- **Overfitting to LoCoMo.** Keep a held-out split and the synthetic bench.
- **Latency tail when stages run in series.** Parallel dispatch, hard deadlines and soft-fail limit it.
- **API churn from Jev's early features.** Use only Noul, Choice and Score. Avoid Box.
- **Injection through stored content into the judge.** Jev has no tools and only outputs probabilities, so the worst case is a skewed ranking. Shadow ledgers make that visible.

## Team cost-latency
# Cost-Latency Team: Tiered Runtime Judgment for Smriti

## 0. Thesis

Most of the quality gaps in the code are decisions: is this relevant, which relation kind applies, is this a duplicate, should these merge. Today those decisions are made by fixed tables or by asking a generative model to report its own confidence. A discriminative judge (Jev) can make them for about $0.0001 per call. Generation (Claude) is kept for the few places where new text is actually needed, and it always runs async. Opus never runs at runtime.

| Tier | Engine | Where it runs | Allowed on recall hot path? |
|---|---|---|---|
| T0 | Rust rules (today's heuristics, plus a local PII screen) | always, and always the fallback | yes |
| T1 | Jev `typesafe/jev-1.14`, pinned | discriminative decisions | yes, with a hard deadline |
| T2 | Haiku 5.5 ($0.10/$0.50 per M) | write-time enrichment text | no, async only |
| T2b | Sonnet 5.5 ($2/$10 per M) | consolidation synthesis (already exists) | no, background loop |
| T3 | Opus 5.5 ($4/$20 per M) | offline only: bench labelling, auditing judge decisions | never |

If T1 or T2 is down, over budget, or switched off, Smriti behaves exactly as it does today.

## 1. Components and traits

**`src/llm/judge/traits.rs`**, a sibling of `Embedder` that follows the same `Arc<dyn …>` and factory pattern:

```rust
#[async_trait]
pub trait Judge: Send + Sync + 'static {
    fn fingerprint(&self) -> JudgeFingerprint;          // provider/model/version
    fn egress(&self) -> Egress;                         // Local | Remote
    async fn ask(&self, state: &JudgeState, qs: &[Question]) -> Result<Vec<Answer>>;
}
pub enum Question { Noul{key: QKey}, Choice{key: QKey, options: Vec<OptKey>}, Score{key: QKey, levels: u8} }
pub enum Answer   { Noul{p: f32}, Choice{probs: Vec<f32>, conf: f32}, Score{pos: f32, probs: Vec<f32>} }
```

- **`JudgeState`** can only be built through `JudgeState::from_memories(&[MemorySummary])` or `::from_query(..)`. That constructor returns `Err(KbError::SensitiveEgress)` for any `Visibility::Sensitive` input when `egress()==Remote`. This makes privacy a type-level guarantee, not something each call site has to remember. `QKey` is an enum, so prompt templates are versioned, static strings. No raw prompt string is ever logged.
- **Implementations:**
  - `JevJudge`: the `typesafe-jev` 0.2.0 crate, behind a cargo feature. Check the crate name before adding it; similarly named crates exist.
  - `NullJudge`: always answers "abstain", so the caller uses T0.
  - `ReplayJudge`: reads the ledger, for deterministic benches and tests.
  - `LocalJudge` later (Laya, self-hosted). This is the only judge that may ever see Sensitive memories.
- **`build_judge(cfg)`** is the factory. The judge has its own circuit breaker, separate from the sidecar's (the same pattern as ADR-026's second `SidecarClient`).
- **`judge_decision` ledger** (new numbered migration). It records memory and query IDs, `QKey`, the answer probabilities, latency, the fingerprint and `request_id`. It never stores content. This allows calibration, replay, shadow mode and rollback.
- **Reranker:** add a `Reranker::Jev` variant to the existing enum (`src/db/reranker/mod.rs`). It keeps the ADR-017 soft-fail contract, so the reranker architecture stays as it is.

## 2. Integration points, by decision

**Recall path** (budget: +≤120 ms at p95, enforced with `tokio::time::timeout`)

| # | Decision | Where | Tier and question | Fallback |
|---|---|---|---|---|
| R1 | Query understanding: Choice over `MemoryType` (5); Noul "has a temporal scope"; Choice over ≤8 candidate entities from alias FTS on the query | `search::recall()` `search.rs:926`, before BM25. Runs concurrently (`tokio::join!`) with `embed_query`, so the extra latency is roughly max(), not a sum | T1, one request with three parallel questions | caller-supplied `person` and `type`, as today |
| R2 | Relevance rerank: Score(5) per candidate over the top 20, all in one request | `Reranker::Jev` at the existing rerank stage | T1 | input order |
| R3 | Rerank gate | `should_rerank` `search.rs:857` | T0 fix: remove the Semantic-only force branch, which means episodic, procedural and prospective queries of 4–6 words are never reranked | n/a |

R1 output is used only as a **soft boost**: inferred entities feed `is_entity_anchored` and the BM25 fetch-doubling. It never acts as a hard filter, so a misclassification cannot hide a true positive.

**Write path** (everything is async through `tasks.spawn` except W0 and W3)

| # | Decision | Where | Tier |
|---|---|---|---|
| W0 | Local PII and secret screen; on a hit, upgrade visibility to Sensitive | `remember::execute()` `remember.rs:100`, **before** the Voyage embed (today the contextual text leaves the machine before visibility is decided) | T0 |
| W1 | Typed relations: pass the top-8 `auto_link` similar memories as candidates, then ask a Choice over {none, 10 relation kinds} for each candidate. Choice probability replaces the self-reported confidence | `llm/extractor.rs::extract_and_persist` is replaced by `judge_relations`, called after the vector is stored (`remember.rs:399-416`) | T1. This removes the Claude call for relations entirely and fixes the structural reason it produces almost no edges (it never sees candidates) |
| W2 | Importance Score(5), is_static Noul | async UPDATE after create; still clamped by `enforce_importance`; recorded as `importance_source=judge` | T1 |
| W3 | Dedup gray band 0.82–0.92: Noul "the same claim, restated" | only inside the warn band, before `create_with_dedup_atomic`, with a 300 ms deadline | T1. It can only escalate warn→reject (with a pointer to the existing ID). The atomic 0.92 cosine gate stays authoritative |
| W4 | Search keys: canonical absolute dates, entity mentions, 3 paraphrased questions → new FTS-indexed `search_keys` field. Also real per-section chunk summaries (`chunking.rs:302-307`) | sidecar, after create, behind a migration | T2 Haiku |
| W5 | Anchor validation: Choice over the `auto_link::extract_candidates` spans | replaces generative `extract_anchors` | T1 |

W4 is aimed directly at the "Fidelity Before Structure" finding: summaries written by the caller lose information, and there is no query expansion on the recall side.

**Lifecycle** (background)
- **C1:** a Noul "same underlying fact or event" check before the Sonnet call in the auto-consolidation loop (`main.rs:~208`). Clusters with p<0.5 skip Sonnet.
- **C2:** a Noul "B contradicts A" check on new writes against their top-K similar memories, raising an alert in `alerts.rs`. It stays detect-only.
- **Gating rule** (from the memory-poisoning research): judges and generators may only *add* edges, raise visibility, and annotate. They may never archive, retract or lower anything.

## 3. Config and kill switches

```toml
[judge]
enabled = false            # global
provider = "jev"           # jev | null | replay | local
model = "typesafe/jev-1.14" # pinned; fingerprint checked against the ledger at startup (warn, not refuse)
allow_remote = false       # explicit opt-in, mirrors the ADR-016 paid-tier gate; Jev retention policy is unverified
recall_deadline_ms = 120
write_deadline_ms = 300
daily_usd_cap = 0.50       # trips the breaker to NullJudge
max_consecutive_failures = 5
shadow = true              # compute and record in the ledger, never act
[judge.features]           # each one can be switched off on its own
query_understanding=false; rerank="voyage"; relations=false; dedup_grayband=false
importance=false; anchors=false; consolidation_pregate=false; contradiction=false
[generation]
enrichment = false; enrichment_model = "claude-haiku-5-5"; consolidation_model = "claude-sonnet-5-5"
```

Environment overrides: `SMRITI_JUDGE_DISABLE=1` and `SMRITI_GEN_DISABLE=1`. `kb_status(include_diagnostics)` gains a judge block: circuit state, spend today, p95 per `QKey`, abstain rate.

## 4. Cost and latency estimates (planning numbers)

| Path | Tokens | Cost | Latency added |
|---|---|---|---|
| Recall R1 | ~300 in | $0.000013 | ~0 (overlaps the embed), ≤120 ms cap |
| Recall R2 (20 × 120 tokens) | ~2.6k in | $0.00011 | replaces Voyage rerank, +0–100 ms |
| Write W1+W2+W5 | ~1.8k in | $0.000076 | 0 (async) |
| Write W3 (gray band only) | ~300 in | $0.000013 | ≤300 ms, on roughly a minority of writes (assumed, not measured) |
| W4 Haiku | 1.5k in / 300 out | ~$0.0003 | 0 (async) |
| C1-gated Sonnet merge | 2k in / 500 out | ~$0.009 per cluster | background |
| Opus bench labelling | 500 probes × 4k in | ~$10 per labelling run | offline |

At 1,000 writes and 1,000 recalls a day, the total is about **$0.50/day**, and most of that is Haiku. Two caveats:
- The sidecar runs on the Agent SDK, which may carry a large fixed system prompt per call. Measure that per call. If it dominates, move T2 to direct Messages API calls with prompt caching inside the same sidecar process.
- The $0.042/M Jev price is unverified against the official page.

## 5. Rollout, one bench gate per step

1. **Fix measurement first.** The `bench/` directory is missing from this checkout, so it has to be restored. Then get a LoCoMo baseline that reproduces within ±1 pt over two runs, and A/B MMR on and off. *Gate:* a stable baseline. Without it, none of the later steps can be evaluated.
2. **Free fixes:** R3, plus W0. *Gate:* no LoCoMo regression, and the PII screen catches ≥95% of a labelled sample.
3. **Judge trait, NullJudge, ledger and ReplayJudge.** No behaviour change. *Gate:* tests pass, 0 clippy warnings, recall p95 within +2%.
4. **R2 in shadow mode, then live.** *Gate:* Recall@1 ≥ +5 pts over Voyage, Jev's scores calibrated on the ledger (ECE <0.1 against Opus labels), p95 ≤ +100 ms.
5. **W1 (relations with candidates).** *Gate:* the typed-edge rate goes from about 0 to a measurable level; precision ≥0.8 on an Opus-audited sample; Recall@10 does not regress through spreading activation.
6. **R1.** *Gate:* Recall@10 ≥ +3 pts, and the temporal and multi-session LoCoMo subsets do not regress.
7. **W4 Haiku enrichment** (migration). *Gate:* retrieval miss rate drops from 44% to <35%, and write cost stays within budget.
8. **W2, W3, W5, C1, C2.** *Gates:*
   - false-merge rate below 2% on labelled duplicate pairs;
   - Sonnet consolidation calls down ≥50% with no loss in merge quality;
   - alert precision ≥0.7.

## 6. Risks

- **Vendor maturity.** Jev is about six weeks old, its calibration claims come from the vendor, and its retention policy is unverified. Mitigations: remote egress is off by default, Sensitive data never leaves the machine (enforced by the type), shadow mode comes before acting, and LocalJudge is the exit path.
- **Version drift.** Box and similar features "can change between releases." Mitigations: pin the version, record the fingerprint in the ledger, and re-run the gate on every model bump.
- **Injection.** Memory summaries become judge input. Jev cannot generate text, so an injected summary can only skew probabilities, and the add-only rules bound the damage.
- **Tail latency and correlated outages** (Voyage and Jev are both remote). Deadlines and per-feature breakers degrade to T0, which is today's system.
- **Cost runaway.** The daily cap trips the breaker to NullJudge.
- **Mis-inference hurting recall.** R1 is a boost only, never a filter. W3 can only escalate. W2 stays clamped by the importance floors.

## Team lifecycle-integrity
# Lifecycle-Integrity Architecture Proposal: Runtime Judges in Smriti

## 1. Thesis

Smriti's integrity problems are all *decisions* currently made by floats and lookup tables: dedup bands (`dedup.rs:10-11`), self-reported sidecar confidence (`extractor.rs:64`, `main.rs` merge gate), detect-only contradictions (`alerts.rs:174-229`), subtype-only decay immunity (`prune.rs:13,107`). These are binary/ordinal/categorical questions over 1–12 short summaries, which is exactly Jev's shape (Noul/Choice/Score, parallel, no generation, ~200 tokens per call). Generation (Claude via the existing sidecar) stays only where text must be produced: consolidation synthesis and procedural distillation. Every judge decision is **advisory-then-gated**: judges create edges, alerts, tags, and ledger rows; they never archive, retract, or delete. That one invariant is the poisoning defense.

## 2. Components

### 2.1 `Judge` trait (`src/db/judge/traits.rs`, alongside `Embedder`)

```rust
pub enum Question {
    Noul { key: &'static str, text: String },
    Choice { key: &'static str, text: String, options: Vec<String> },   // <=255
    Score { key: &'static str, text: String, levels: u8 },              // 2..=10
}
pub enum Verdict {
    Noul { p_yes: f32 },
    Choice { probs: Vec<f32>, confidence: f32 },
    Score { expected: f32, probs: Vec<f32>, confidence: f32 },
}
#[async_trait]
pub trait Judge: Send + Sync + 'static {
    fn fingerprint(&self) -> JudgeFingerprint;              // provider/model/version, stored per verdict
    fn max_state_bytes(&self) -> usize;                     // Jev: stay <32KB, avoid long-context mode
    async fn ask(&self, state: &str, qs: &[Question]) -> Result<Vec<Verdict>>;
}
```

Impls: `JevJudge` (crate `typesafe-jev` 0.2.0, `Client::from_env`, pinned `typesafe/jev-1.14`, own `AdaptiveLimiter`), `SidecarJudge` (same questions via JSON-schema on the existing `SidecarClient`, for ops who refuse a third vendor), `NoopJudge` (returns `Err(JudgeError::Disabled)`; callers fall through to today's heuristic). `build_judge(cfg) -> Arc<dyn Judge>` mirrors `build_embedder`. `state` is built from summaries only, never `content` (same injection posture as `remember.rs:441`), and `Visibility::Sensitive`/`Private` memories are excluded exactly as the sidecar does.

### 2.2 `judge_verdict` ledger (Migration 016)

Generalizes `consolidation_attempt`: `{request_id|operation_id, kind, subject_ids, question_keys, verdicts, fingerprint, latency_ms, action_taken, created_at}`. Every judge call writes one row *before* any action. This is the audit trail, the offline replay set for calibration, and the input for shadow-mode comparison. No summary text stored; IDs only.

### 2.3 `JudgeOutcome` enum (tool-gating)

`CreateEdge(TypedRelation)`, `RaiseAlert(Alert)`, `Tag(MemoryId, Tag)`, `SkipMerge`, `AllowMerge`. There is deliberately no `Archive`/`Retract` variant; the compiler enforces that judge and sidecar output cannot destroy memory (Memory-Sandbox finding, arXiv 2605.08442).

## 3. Integration points and model tier

| # | Decision | Where | Tier | Why this tier |
|---|---|---|---|---|
| A | **Contradiction/supersession classification**: for dedup *warn* band hits (0.82–0.92, same author, `memory.rs:create_with_dedup_atomic` returns the neighbour) and for explicit `updates_id` (`remember.rs:242-270`) | post-create task in `remember.rs` (`tasks.spawn`, after vector store) | Jev Choice `{restatement, update, contradiction, distinct}` + Noul "is the older claim still true?" | Categorical over two summaries; cosine cannot tell restatement from reversal |
| B | **Consolidation pre-gate** | `main.rs` tick, between `find_candidates` and `synthesize_consolidation` | Jev Noul "do these summaries describe one topic?" + Noul "time-conflicting facts present?" | Cuts sidecar spend ($0.05/call) by rejecting bad clusters for ~$0.00001; temporal-conflict clusters are skipped and alerted (MemoTime/TSM finding) |
| C | **Consolidation fidelity gate** | `main.rs` merge decision, replacing sidecar self-reported `confidence >= min_confidence` | Jev Score (5 levels) "merged summary preserves every fact in sources" + Noul "introduces a fact absent from sources" | Replaces uncalibrated generative confidence with a discriminative one; `min_confidence` config reused as the Score threshold |
| D | **Reconsolidation on retrieval** (REALM analog) | `search.rs:1567`, fire-and-forget beside SM-2 | Jev Choice over the 10 `typed_relation` kinds + Score confidence, for returned pairs with cosine 0.70–0.85 and no existing edge | Edges strengthened by co-retrieval, not only creation; feeds spreading activation; cheap enough per recall |
| E | **Procedural distillation** (ReasoningBank) | new periodic task in `main.rs`; reads `episodic/session|daily_log` ≥7d old | Jev Noul gate "contains a reusable lesson or failure?" → Claude Sonnet sidecar generates `semantic/pattern` or `procedural/workflow` with `derives` edges and `outcome:success|failure` tag → Jev Score "generalizable beyond this instance" before persist | Generation needed; Jev gates both ends so Sonnet runs on <10% of episodes |
| F | **Poison screen** | write path post-create, before sidecar extraction is spawned (`remember.rs` ~`:441`) | Jev Noul "contains instructions addressed to an AI/agent or asks to modify other memories?" | p≥0.8 → tag `suspect`, skip sidecar extraction and auto-link; memory stays recallable; alert raised |
| G | **Decay/archive guard** | `prune.rs:archive_below_threshold`, batch of candidates | Jev Noul "durable fact worth keeping?" (optional, last) | Replaces subtype-only immunity with content judgment; also wire the dead `domain_decay_rate` (evidence: documented, never invoked) |

Opus/Fable are never on the runtime path; they label the eval set offline. Haiku has no role: Jev is cheaper and cannot be prompt-injected into producing text.

Dependency on the write team: typed-relation extraction must receive top-K `auto_link::find_similar` candidates in the sidecar prompt; otherwise D is the only path that ever produces resolvable `to` IDs.

## 4. Config and kill switches

```toml
[judge]
provider = "none"          # none | jev | sidecar   (default none: local-first, ADR-016 privacy gate)
model = "typesafe/jev-1.14"
api_key_env = "TYPESAFE_API_KEY"
timeout_ms = 1500
max_concurrency = 4
daily_budget_usd = 0.50
shadow = true              # verdicts to ledger only, no actions
[judge.features]
contradiction = false; consolidation_gate = false; reconsolidation = false
distillation = false; poison_screen = false; decay_guard = false
auto_supersede = false     # the only destructive-adjacent feature; requires Noul>=0.95, same author, still emits alert
```

Env `SMRITI_JUDGE_DISABLE=1` overrides all. Judge has its own circuit breaker (third breaker; documented in ADR, isolated from both sidecar clients). Breaker open ⇒ `NoopJudge` behaviour, i.e. today's code path, byte-identical. Fingerprint stored per verdict; model change does not refuse start (unlike embedder) but invalidates calibration and flips `shadow=true` until re-validated.

## 5. Cost and latency

State ≈ 2–12 summaries × ~50 tokens + question text ≈ 200–800 tokens. At $0.042/M: $0.00001–0.00003 per call. Assume 2,000 writes/day (A+F: ~3 questions, one call) + 5,000 recalls/day (D: ~3 pairs) + 5 clusters/day (B+C): ≈ 20k calls ≈ 8M tokens ≈ **$0.35/day**, bounded by `daily_budget_usd`. Sidecar spend *drops* (B gates it). Latency: every call is off the caller's hot path (post-create spawn, post-recall spawn, periodic ticks), so recall p95 and remember p95 are unchanged; ledger write adds one insert per verdict. Jev 70–500ms is irrelevant to MCP callers. Long-context mode (>32KB) is avoided by `max_state_bytes` with cluster truncation to `max_cluster_size`.

## 6. Eval harness (prove each step)

1. **Restore `bench/`** (gitignored, absent) and run LoCoMo + synth-oracle baseline before any code; fix the oracle; record Recall@1/@10, answer accuracy, temporal and multi-hop subsets separately.
2. **Labeled judge set** `bench/judge/`: ≥300 memory pairs labeled `{restatement, update, contradiction, distinct}`, ≥100 consolidation clusters labeled `merge/no-merge` with fidelity scores, ≥100 poison/clean summaries. Labeled offline by Opus, spot-checked by hand. Metrics: accuracy, precision per class, ECE calibration (vendor "0.9 means 90%" is unverified).
3. **Shadow replay**: `judge_verdict` ledger lets any feature run in production for a week with zero actions, then compared offline.
4. **Live quality counter**: extend `recall_metrics.rs` with judge-derived edge hit rate (did a D-created edge contribute a returned result) alongside latency histograms.

## 7. Rollout order (each step gated)

| Step | Ships | Gate |
|---|---|---|
| 0 | `Judge` trait, `NoopJudge`, Migration 016, config, bench restored, baseline | tests green, 0 clippy, no behaviour change, baseline numbers committed |
| 1 | `JevJudge` in shadow for A, B, C, F | ECE <0.10; agreement with labels ≥85%; budget guard proven |
| 2 | C live (fidelity gate replaces self-confidence), B live | LoCoMo no regression; merged-cluster recall probes ≥ baseline; sidecar call count −50% |
| 3 | A live (alerts + `updates`/`retracts` *suggestion* edges), F live | contradiction precision ≥0.90; poison recall ≥0.95 on labeled set; zero destructive actions by construction |
| 4 | D live | Recall@10 +2 pts on multi-hop subset; recall p95 unchanged; hub-cap respected (edges tagged `source=judge`) |
| 5 | E live | pattern/workflow probe set; distilled memories retrieved for ≥30% of matching action-item queries |
| 6 | G, then `auto_supersede` opt-in | each on its own labeled gate; auto_supersede only after A has ≥4 weeks of ledger with ≥0.95 precision |

MMR λ=0.7 is rolled back to `enabled=false` at step 0 (ADR-031's own escape) until the restored bench measures it.

## 8. Risks

- **Calibration is vendor-claimed**: mitigated by shadow mode, ECE gate, ledger replay; `SidecarJudge` is the fallback if Jev fails calibration.
- **Privacy**: Jev is US-hosted, retention unclear, no SOC 2 — summaries leave the machine. Same opt-in gate as Voyage (ADR-016); Sensitive/Private never sent; default `provider=none`.
- **Feedback loops**: judge-created edges feed spreading activation, which feeds D. Cap D at N edges per memory per day, tag `source=judge`, respect `involves_hub_cap`.
- **Three circuit breakers**: documented surface growth; justified by independent failure domains.
- **Vendor drift**: Box/image are "early"; pin 1.14, fingerprint per verdict, `TokenLimit` error ⇒ split cluster.
- **Bench self-contamination** (ADR-021): the labeled judge set must be built from a corpus the judge never sees in production.
- **False contradictions on legitimately changed preferences**: A raises alerts, never archives; the Noul "older still true?" verdict is shown in the alert so a human/agent decides.

Referenced files: `/Users/dhruv/projects/system/smriti/src/db/embeddings/traits.rs`, `src/db/dedup.rs`, `src/db/memory.rs:285,635,834`, `src/db/consolidation.rs:63,380`, `src/db/consolidation_ledger.rs`, `src/db/alerts.rs`, `src/db/prune.rs`, `src/db/search.rs:1567`, `src/llm/sidecar.rs:231,337`, `src/llm/extractor.rs:26`, `src/tools/remember.rs`, `src/main.rs:516,601`, `src/config.rs:281`.

# Debate

# Cross-team critique and ruling

## Code checks that change the debate (all verified)

- `search.rs:949` `fetch_limit = limit.min(50)`; `:1147` `primary_cap = limit-3`; `:1195` MMR `pool_out = memories.len()` (reorders an already-truncated list); `should_rerank` ends in `matches!(Semantic)`. All three teams are right about these.
- `sidecar/src/index.ts:148-154,215-245`: relation prompt carries no candidate IDs; `extractor.rs:64` gates on self-reported confidence. Typed-relation extraction is dead by construction. Confirmed.
- **Sidecar is hardcoded to `claude-sonnet-4-5`** (`index.ts:234,302,377`), not config-driven. Every team's "Haiku 5.5 / Sonnet 5.5" tier table assumes a model knob that does not exist.
- **Sensitive memories already leave the machine**: `remember.rs:100-124` embeds via Voyage *before* visibility is computed (`:230-240`), and no `Sensitive` gate exists in the embed path (grep: only the sidecar is gated at `:453`). Quality-First's "Sensitive never leaves the machine" is already false today.
- Voyage rerank *does* see `content` (3200 chars, `voyage.rs:25`). "Content never indexed" holds only for the embed/BM25 candidate stage, which is exactly where the 44% miss lives.
- `domain_decay_rate` has one reference: its definition. Dead.
- `bench/` is gitignored and absent. `STATUS.md`: **samples=1, 145 probes, 196 oracle QAs**, Recall@1 = 24/196, judge = Claude Code Max, not Opus. Conditional accuracy when hit is 76.5%, so retrieval is the ceiling. Recall p50 is already 1167 ms with Voyage rerank (+314 ms tax).

## Team Quality-First

**Q1. Your gates (Recall@1 +8 pts, Recall@10 +3 pts) are inside the noise floor of n=196, samples=1.** A 3-point delta on 196 items is ~6 QAs; 95% CI on 50% at n=196 is ±7 pts.
*Steel-man:* "We said 3 runs with a held-out split, and step 1 is 'baseline reproducible within ±1 pt'." *Verdict: objection stands.* Three runs of the same 196 QAs do not shrink the oracle's sampling error, only the pipeline's nondeterminism. Gates need either the full LoCoMo (all 10 conversations, ~1,900 QAs) or paired per-query significance tests. Nobody proposed either.

**Q2. Latency: you put Jev judge (150-500 ms) *after* Voyage (+314 ms) on the hot path, plus Haiku decomposition (0.5-0.9 s) on 25% of queries.** Recall p95 goes from 1.3 s to ~2 s, 2.5 s on multi-hop. For an MCP tool called several times per agent turn, that is user-visible.
*Steel-man:* "Latency is the constraint, we said so; 600 ms deadline, soft-fail." *Verdict: partly stands.* Deadlines bound the tail but not the median. Ruling: Jev relevance judge should **replace** Voyage rerank behind `Reranker::Jev` (Cost-Latency R2), not stack on it, and be A/B'd. Decomposition stays off until R1/R2 are measured.

**Q3. Enrichment (C) adds a migration, a second vector, a backfill tool, and Haiku generation per write, before anyone has shown pool recall is the bottleneck vs. embedding quality.** Step 3 (embed `summary+content`) might capture most of the gain for zero new schema.
*Steel-man:* "Enrichment is step 5, after B is measured; `event_time_norm` is what LoCoMo temporal questions need and content embedding won't give it." *Verdict: does not stand for the temporal field; stands for `enrich_questions`/second vector.* Ruling: ship `event_time_norm` + canonical entity names as one FTS field (Cost-Latency's `search_keys`), defer the second vector.

## Team Cost-Latency

**Q1. "+≤120 ms p95" on recall is not credible with a remote judge whose vendor-quoted range is 70-500 ms.** R2 over 20 candidates is one request, but the tail is the vendor's tail.
*Steel-man:* "R1 overlaps the embed; R2 replaces Voyage's 314 ms, so net is negative at the median; timeout returns input order." *Verdict: stands in part.* The number should be stated as a **replacement** budget (≤ Voyage's current tax), not an absolute. Also: on timeout, R2 returns *unreranked* order, which is a regression from today's Voyage path. Ruling: on Jev timeout, fall back to Voyage, not to input order.

**Q2. W1 removes the Claude relation call entirely in favour of Jev Choice over top-8 cosine neighbours. Cosine neighbours are topically similar; `causes/blocks/depends_on` edges are often between *dissimilar* memories.** You fix the resolvability bug but cap recall of relations at what embeddings already surface.
*Steel-man:* "Today's edge count is ~0; anything measurable beats that, and spreading activation already walks `relates` edges built from the same cosine neighbourhood." *Verdict: objection stands as a ceiling, not a blocker.* Accept W1 as phase 1; log pairs where all 11 options are near-uniform so the gap is visible.

**Q3. W0 (local PII screen upgrades visibility to Sensitive) is T0 "Rust rules", but no such screen exists, and regex PII detection has a well-known precision problem; false Sensitive upgrades silently remove memories from the sidecar, consolidation and (if adopted) judge paths.** Also your ≥95% gate has no labelled set.
*Steel-man:* "It is the only team noticing that Sensitive data already egresses via Voyage at `remember.rs:110`; even a crude screen is better than none." *Verdict: the finding stands and is the single most important privacy item across all teams; the mechanism needs a design.* Ruling: W0 should *warn and tag* in phase 1, not auto-upgrade; and the embed call must be moved after visibility resolution and gated on `Sensitive`, which is a one-function reorder in `execute()`.

## Team Lifecycle-Integrity

**Q1. Everything is off the hot path, so by your own design nothing you ship can move Recall@10, the metric the user cares about.** D (reconsolidation edges) is the only retrieval-touching item and it feeds spreading activation, which `primary_cap` currently taxes at 3 slots.
*Steel-man:* "Integrity is a precondition: false dedup rejects and unconsolidated clusters are *why* the pool is missing items; contradiction alerts stop stale facts from outranking fresh ones." *Verdict: stands.* The team's deliverables are valuable but should not be sequenced ahead of pool fixes. Their step-0 "MMR off" is correct and nobody else said it.

**Q2. Judge state is "summaries only, never content", for injection parity with the sidecar. But Jev cannot generate, so the injection surface is probability skew, which your add-only `JudgeOutcome` already bounds. You are paying a fidelity cost (A, C decisions on 500-char summaries) for a defence you already have.**
*Steel-man:* "Content is up to 500 KB; `max_state_bytes` <32 KB keeps us out of Jev's slow long-context mode." *Verdict: partly stands.* Ruling: send a bounded content prefix (Quality-First's 600 chars) for A and C; the 32 KB cap is the real constraint, not injection.

**Q3. 20k Jev calls/day for 2,000 writes and 5,000 recalls is an assumed load; this is a single-user local-first server.** The spend is fine, but D's per-recall spawn adds DB writes (ledger + edges) to every `kb_recall`, on a SurrealKV where `array::group` already misbehaves (`consolidation.rs:52`).
*Steel-man:* "Budget-capped, per-memory edge cap, `source=judge` tag." *Verdict: stands mildly.* Batch D into the hourly prune tick from a co-retrieval log rather than per-recall spawns.

## Consensus (all three)

1. Restore `bench/`, fix the oracle, get a stable baseline before any LLM. Add a quality counter beside latency in `recall_metrics.rs`.
2. `Judge` trait as `Arc<dyn Judge>` + `build_judge`, mirroring `Embedder`; `Null/Noop` impl; own circuit breaker; pinned `typesafe/jev-1.14`; no Box.
3. Ledger of verdicts (IDs, probabilities, fingerprint, latency, never content); shadow mode before live.
4. Relation extraction must receive candidate IDs; replace generative confidence with discriminative Choice/Score.
5. Dedup gray band 0.82-0.92 gets a Noul; the 0.92 atomic gate stays authoritative.
6. Judges never archive/retract; add-only outcomes.
7. Remove the `Semantic`-only force branch; fix `primary_cap`, pool size, MMR ordering, `recall_at` parity.
8. Opus/Fable offline only.

## Conflicts and rulings

| Conflict | Ruling |
|---|---|
| Judge sees content? (QF: 600-char snippet; CL: summaries; LI: summaries only) | 600-char prefix, bounded by 32 KB state. |
| Jev rerank stacks on Voyage (QF) vs replaces it (CL) | Replace via `Reranker::Jev`, Voyage as timeout fallback. |
| Haiku role: enrichment (QF, CL) vs none (LI: "Jev cannot be injected into producing text") | Haiku for `event_time_norm` + entity canonicalisation only; output is index-only, never returned as memory text. LI's concern is answered by that invariant. |
| Hot-path Jev at all (QF, CL yes; LI no) | Yes for R1/R2 only, after step-2 pool fixes are measured. |
| Sidecar `confidence` gate: keep (QF) vs replace with Score (CL, LI) | Replace. |
| MMR: keep λ=0.7 and re-measure (QF) vs disable at step 0 (LI) | Disable at step 0 (ADR-031's own escape), re-enable only after bench. |
| Trait location: `src/llm/judge` (QF, CL) vs `src/db/judge` (LI) | `src/llm/`, it is an external model client. |

## Missed by everyone

- **Sidecar model is hardcoded** (`index.ts:234,302,377`); all tier tables need a config knob first.
- **Voyage embedding of `Sensitive` memories** is live today; only CL noticed obliquely. Move embed after visibility.
- **n=196, samples=1** makes every proposed gate unmeasurable. Use full LoCoMo and paired tests.
- **Conditional accuracy is 76.5% when hit**: even perfect retrieval caps answer accuracy near 76% with the current caller LLM; nobody budgeted for the answer-side ceiling.
- **Rerank already reads content** but embedding does not. The cheapest fidelity win is Quality-First step 3 alone; it needs no new vendor and may make most Jev proposals moot for Recall@10. Run it first.
- **Jev calibration** must be measured on Smriti's own labels (ECE); LI alone made this a hard gate. Adopt for all judge uses.