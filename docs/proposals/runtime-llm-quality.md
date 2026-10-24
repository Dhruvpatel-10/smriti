# Smriti Runtime-LLM Quality Architecture: Final Decision

**Status:** Decided · **Date:** 2026-10-24 · **Scope:** write path, recall path, lifecycle

## 1. Diagnosis

Retrieval is the bottleneck. It has two separate faults: gold evidence never enters the candidate pool, and when it does enter, it ranks badly. Using LLMs well is a smaller concern. The faults are listed in order of evidence strength.

| # | Finding | Evidence |
|---|---|---|
| D1 | Retrieval limits answer quality | LoCoMo accuracy is 63.4% overall but 76.5% when the gold memory is retrieved. The miss rate is 44%. |
| D2 | Pool loss before any ranking | `search.rs:949` caps the pool with `fetch_limit = limit.min(50)`. `:1147` sets `primary_cap = limit-3`, which takes three answer slots for graph neighbours. Gold that ranks below 50 in both BM25 and vector cannot be recovered. |
| D3 | The text used for search is too thin | The embed text (`remember.rs:22,111`) is `[type/subtype][project][domains] summary`. Content under 2000 bytes is never embedded. The Voyage reranker reads 3200 chars of content (`voyage.rs:25`), but the candidate stage where the misses happen does not. This matches "Fidelity Before Structure" (arXiv 2601.00821). |
| D4 | Ranking failure | Recall@10 is 50% but Recall@1 is 12.2%. `should_rerank` ends in `matches!(Semantic)` (`:908`), so episodic, procedural and prospective queries of 4–6 words are never reranked. MMR reorders a list that is already truncated (`:1195`), so it cannot recover anything. It shipped unmeasured. |
| D5 | Typed relations are broken by design | The sidecar prompt contains no candidate memory IDs (`sidecar/src/index.ts:148-154,215-245`), so `to` cannot resolve. The confidence gate (`extractor.rs:64`) uses the model's own self-reported confidence. In practice, almost no edges get persisted. |
| D6 | `recall_at` is weaker than `recall` | It has no person filter, alias expansion or spreading activation (`:1583-1847`). Temporal LoCoMo questions take this weaker path. |
| D7 | Privacy bug | `execute()` embeds through Voyage (`remember.rs:109-112`) before visibility is resolved (`:230-240`). `Sensitive` memories leave the machine today. |
| D8 | Measurement is broken | `bench/` is absent from this checkout. The synthetic oracle is broken. STATUS.md shows samples=1 and n=196 QAs, so the 95% CI on a 50% recall is about ±7 pts. Every gain the teams proposed is inside that noise. Production telemetry measures latency only. |
| D9 | The tier tables assume a model setting that doesn't exist | The sidecar model is hardcoded to `claude-sonnet-4-5` (`index.ts:234,302,377`). |
| D10 | Dead code and blind defaults | `domain_decay_rate` (`prune.rs:21`) is never called. Importance, `is_static`, visibility and dedup are all fixed tables or a single global float threshold. |

**Conclusion.** Fix measurement first. Then make the free pool and fidelity fixes, which need no LLM. Only then add LLM judgment, using the cheapest model that can make each decision. Most quality decisions here are classifications, which suits Jev. Generation is limited to async write-time indexing and consolidation.

## 2. Target architecture

```
                       WRITE (kb_remember)
caller ─► agent resolve ─► validate ─► VISIBILITY RESOLVE ─► [W0 PII screen: warn+tag]
                                              │
                         Sensitive? ──yes──► local-only path (no Voyage, no Jev, no sidecar)
                                              │ no
                        embed_document(summary + content prefix)   ◄─ D3 fix
                                              │
                    cosine dedup ─ >0.92 reject (authoritative)
                                 ─ 0.82–0.92 ─► [W3 Jev Choice: restatement|update|contradiction|distinct, 300ms]
                                              │
                                           CREATE
                                              │ async (tasks.spawn)
          ┌───────────────┬──────────────────┼─────────────────────┬─────────────────┐
     vector+auto_link  [W1 Jev relations   [W5 Jev anchors over   [W4 Haiku search_keys:  entity link
                       over top-8 cands]   extract_candidates]    event_time_norm +
                                                                  canonical entities → FTS]
                                              │
                                   judge_verdict ledger (IDs + probs only)

                       RECALL (kb_recall / kb_recall_at, same pipeline)
query ─┬─► embed_query ─────────────┐
       ├─► [R1 Jev: type, temporal, person-candidate]  (tokio::join!, soft boosts only)
       └─► BM25 (summary|content|anchors|search_keys) ─┤
                                    vector ────────────┤
                         RRF over pool_size=150 (+ R1 person-anchored list)
                                    │
                  spreading activation neighbours join the pool (no reserved slots)
                                    │
                [R2 Reranker::Jev  Score(5) × top-30, deadline] ─timeout─► Voyage
                                    │
                    MMR on the expanded pool (only if the bench approves) ─► truncate to limit
                                    │
                    + evidence_sufficiency (Jev Noul, same request) ─► caller

                       LIFECYCLE (background ticks)
find_candidates ─► [C1 Jev pre-gate] ─► Sonnet synthesis ─► [C2 Jev fidelity Score] ─► merge
co-retrieval log ─(hourly)─► [D Jev relation Choice] ─► add-only edges (source=judge)
```

**Invariants:**

- Judges and generators only return `JudgeOutcome::{CreateEdge, RaiseAlert, Tag, SkipMerge, AllowMerge}`. There is no Archive or Retract variant, so they can never destroy memory, and the compiler enforces it.
- `JudgeState` construction rejects `Sensitive` input when the judge's egress is Remote.
- Generated `search_keys` are used only for indexing. They are never returned as memory text and never fed into dedup.
- If any judge is disabled, times out or trips its breaker, Smriti behaves exactly as it does today.

## 3. Model-tier routing (every runtime decision)

Jev's price ($0.042/M input, output free) is unverified against the official page. Claude per-call costs are planning estimates.

| Decision | Engine | Where | Est. cost | Added latency |
|---|---|---|---|---|
| Summary/content/type validation, TTL, importance bounds | Rule | `validation.rs` | 0 | 0 |
| PII/secret screen (W0) | Rule (local) | `execute()` before embed | 0 | <1 ms |
| Dedup >0.92 | Rule (cosine) | `dedup.rs` | 0 | 0 |
| Dedup gray band 0.82–0.92 (W3) | Jev Choice(4) | before `create_with_dedup_atomic` | ~$0.00002 | ≤300 ms, band only |
| Typed relations (W1) | Jev Choice(11) × top-8 candidates | replaces `extract_and_persist` | ~$0.00005 | async |
| Anchor entities (W5) | Jev Choice over `extract_candidates` spans | replaces generative `extract_anchors` | ~$0.00001 | async |
| Importance and `is_static` (P3) | Jev Score(5) + Noul, still clamped by floors | async UPDATE | ~$0.00001 | async |
| Search keys: absolute dates + canonical entities (W4) | Haiku 5.5 | sidecar, post-create | ~$0.0003/write | async |
| Per-chunk summaries | Haiku 5.5 | `chunking.rs:302-307` | ~$0.0002/chunk | async |
| Query understanding (R1) | Jev, 3 parallel questions | `recall()` start, joined with the query embed | ~$0.00001 | ~0 net, 150 ms cap |
| Relevance rerank (R2) | Jev Score(5) × 30 | `Reranker::Jev` | ~$0.0004/query | replaces Voyage's +314 ms |
| Rerank fallback | Voyage | existing | as today | as today |
| Evidence sufficiency | Jev Noul (in the R2 request) | response field | ~0 | 0 |
| Multi-hop decomposition | Haiku 5.5 (P3, off) | after R1 | ~$0.0001 | 0.5–0.9 s |
| Consolidation pre-gate (C1) | Jev Noul × 2 | `main.rs` tick | ~$0.00001 | background |
| Consolidation synthesis | Sonnet 5.5 (needs the config setting) | sidecar | ~$0.01/cluster | background |
| Merge fidelity (C2) | Jev Score(5) + Noul "adds a fact not in the sources" | replaces the self-reported `confidence` gate | ~$0.00002 | background |
| Contradiction detection | Jev Choice (from W3 output plus `updates_id`) | alert only | included in W3 | async |
| Reconsolidation edges (D) | Jev Choice, hourly batch | prune tick | ~$0.0001/batch | background |
| Procedural distillation (P3) | Jev gate → Sonnet → Jev gate | new tick | ~$0.01/accepted episode | background |
| Bench labelling (graded relevance, pairs, clusters) | Opus 5.5 | offline | ~$10–40/run | n/a |
| Label adjudication (Opus vs human disagreements, ~10%) | Fable 5.1 | offline | ~$5/run | n/a |
| Judge calibration audits (ECE) | Opus 5.5 | offline, on each model bump | ~$5 | n/a |

Opus and Fable are never on the runtime path. Haiku is used only where new text is needed. Expected spend at 1k writes and 1k recalls per day is under $1/day, mostly Haiku. The real constraint is latency, not cost.

## 4. Roadmap

Each step ships behind its own config flag. A step that fails its gate is switched off in config, with no code revert. All gates use the eval of §5: full LoCoMo10, a paired test with p<0.05, and at least the stated effect size.

### P0: Measurement, privacy, free fixes (no LLM)

| Item | Integration point | Gate | Kill switch |
|---|---|---|---|
| P0.1 Restore `bench/`, fix the oracle, add stage-wise gold tracking (BM25@N, vector@N, post-fusion, post-rerank, top-k) | `bench/`, `recall_metrics.rs` quality counter | Baseline reproduces within ±1 pt over 3 runs on the full set | n/a |
| P0.2 Turn MMR off (`[mmr].enabled=false`), using the escape ADR-031 already provides | config | none needed | config |
| P0.3 Reorder `execute()` so visibility is resolved before embedding; `Sensitive` gets a local-embedder vector or none | `remember.rs:100-124` | Test: no remote embed call for `Sensitive` | none (privacy fix) |
| P0.4 Sidecar model config (`[sidecar].relation_model`, `consolidation_model`, `enrich_model`) | `sidecar/src/index.ts:234,302,377`, `config.rs` | Tests green | defaults to current |
| P0.5 `[search].pool_size=150` replaces the `min(50)` cap | `search.rs:949,1649` | Recall@10 +3 pts, p95 +≤50 ms | `pool_size=50` |
| P0.6 Remove the `primary_cap` reservation; spreading neighbours compete in the pool | `search.rs:1147-1179,1439,1452` | Recall@10 no regression, Recall@1 up | `[spreading].reserve_slots=3` |
| P0.7 Remove the `Semantic`-only fallthrough in `should_rerank` | `search.rs:857-909` | Recall@1 up on episodic/procedural/prospective subsets | `rerank_policy` |
| P0.8 Embed `summary + content` prefix (truncated to embedder max), run `kb_reembed` via `smriti-librarian` | `build_embed_text` `remember.rs:22` | Gold-in-pool rate up, Recall@10 +3 pts, synthetic bench no regression | `[embed].include_content=false` + reembed |

P0.5–P0.8 are measured one at a time. P0.8 may close most of the Recall@10 gap with no new vendor.

### P1: Judge foundation, ranking, broken relations

| Item | Integration point | Gate | Kill switch |
|---|---|---|---|
| P1.1 `Judge` trait (`src/llm/judge/`), `Arc<dyn Judge>`, `build_judge`; `NullJudge`, `ReplayJudge`, `JevJudge` (`typesafe-jev` 0.2.0, crate name checked, pinned `typesafe/jev-1.14`, no Box, no `-latest`); its own breaker; Migration 016 `judge_verdict` ledger (IDs, QKey, probs, fingerprint, latency, `request_id`, never content) | new module, `main.rs` wiring | Tests green, 0 clippy warnings, recall p95 within +2% with NullJudge | `[judge].provider="none"`, `SMRITI_JUDGE_DISABLE=1` |
| P1.2 `Reranker::Jev` (R2): Score(5) over the top 30, state = query + summary + 600-char content prefix, `w·E[level]+(1−w)·fused`, `w` fitted on the dev split; on timeout, fall back to Voyage. Shadow first, then A/B against Voyage | `reranker/mod.rs` | ECE <0.10 against Opus labels; Recall@1 +5 pts over Voyage; median latency ≤ Voyage's | `[rerank].provider="voyage"` |
| P1.3 Relation candidates (W1): top-8 `auto_link::find_similar` IDs; Jev Choice over {none + 10 kinds}; Choice probability replaces self-reported confidence; log near-uniform answers | `remember.rs:399-416`, `extractor.rs` | Persisted typed edges go from ~0 to measurable; precision ≥0.8 on an Opus-audited sample; Recall@10 no regression | `[judge.features].relations=false` |
| P1.4 `recall_at` parity: person filter, alias expansion, spreading | `search.rs:1583-1847` | Temporal subset up, `recall` unchanged | `[recall_at].full_pipeline=false` |
| P1.5 Re-measure MMR as expand-then-diversify-then-truncate | `search.rs:1181-1201` | Recall@10 no regression at its best λ, otherwise it stays off | `[mmr].enabled` |

### P2: Query understanding, temporal indexing, write integrity

| Item | Integration point | Gate | Kill switch |
|---|---|---|---|
| P2.1 R1 query understanding: Choice(type), Noul(temporal), Choice(person over ≤254 alias-FTS candidates + none). Soft boosts only, plus an extra person-anchored RRF list when p≥0.7 | `recall()` ~`:926-957` | Recall@10 +3 pts on multi-session/temporal; single-hop drop ≤1 pt | `[judge.features].query=false` |
| P2.2 W4 `search_keys` FTS field (Haiku): `event_time_norm` plus canonical entity names; `enrich_model` stored per row; `kb_enrich` backfill | migration 017, BM25 predicate `:1012,1665` | Temporal-category accuracy +5 pts | `[generation].enrichment=false` |
| P2.3 W3 dedup gray band: restatement → reject with the existing ID; update → create + suggested `updates` edge + alert; contradiction → create + `Contradiction` alert; distinct → create. 300 ms deadline, timeout means today's warn | `dedup.rs`, `remember.rs` | False-merge rate <2% and contradiction precision ≥0.9 on labelled pairs | `[judge.features].dedup_grayband=false` |
| P2.4 C1 pre-gate + C2 fidelity gate replace the sidecar's self-reported `confidence` | `main.rs` auto-consolidation tick | Sonnet calls −50%; merged-cluster recall probes ≥ baseline | `[judge.features].consolidation_gate=false` |
| P2.5 `evidence_sufficiency` field (Noul, inside the R2 request) | recall response, additive | Abstention precision ≥0.8 on adversarial LoCoMo | field omitted |
| P2.6 W0 PII screen: warn and tag only, no auto-upgrade | `execute()` | ≥95% recall on a labelled sample; false-tag rate reported | `[privacy].pii_screen=false` |
| P2.7 W5 anchor Choice replaces generative anchors; Haiku per-chunk summaries | `extractor.rs`, `chunking.rs` | Anchor precision ≥ current; chunk recall up | per-feature flag |

### P3: Lifecycle depth (only after P1–P2 are proven)

- **Reconsolidation from co-retrieval.** Log pairs that are retrieved together, then classify them in a batch on the hourly prune tick. No per-recall spawns. Each memory gets at most N edges per day, all tagged `source=judge`, and the hub cap is respected. Gate: Recall@10 +2 pts on the multi-hop subset.
- **Procedural distillation** (ReasoningBank-style): Jev gate, then Sonnet, then a Jev gate. Gate: the distilled pattern or workflow is retrieved for ≥30% of matching probes.
- **Importance and `is_static` judge (W2), decay guard.** Wire or delete `domain_decay_rate`. Delete it unless the bench shows a decay effect.
- **Haiku multi-hop decomposition.** Only if R1 shows multi-hop is still the weak subset.
- **`LocalJudge` (Laya)** as the exit from the vendor and the only judge allowed to see `Sensitive`. Gate: ECE <0.10, since the base model reportedly needs fine-tuning.
- **`auto_supersede`.** Requires 4 weeks of P2.3 ledger data at ≥0.95 precision, the same author, and an alert on every use.

## 5. Eval fixes required before any gate means anything

1. **Restore and version `bench/`.** It is gitignored and missing. Move it to a tracked repo or submodule so every machine runs the same harness.
2. **Use the full LoCoMo10 (~1,900 QAs), not 196.** The 95% CI becomes about ±2.2 pts. Gates use paired per-query tests (McNemar for hit/miss, paired bootstrap for MRR/nDCG). Fix the split: dev for fitting `w` and thresholds, a held-out test split for gates, and the synthetic bench as a regression guard.
3. **Stage-wise gold tracking** so a change can be attributed to pool recall or to ranking.
4. **Graded-relevance oracle (ADR-021), rebuilt offline.** Opus labels about 500 probes × 30 candidates and Fable adjudicates disagreements. The corpus must be disjoint from anything the judge sees in production, to avoid self-contamination.
5. **Judge label sets:**
   - ≥300 memory pairs (restatement, update, contradiction, distinct)
   - ≥100 clusters (merge decision plus fidelity)
   - ≥100 poison or clean summaries
   - relation-edge audit samples

   Report accuracy, per-class precision and ECE. Jev's claim that "0.9 means 90%" is a vendor claim.
6. **Report answer accuracy separately from retrieval.** The caller LLM's answering caps accuracy near 76.5% even with perfect retrieval. Track conditional accuracy so retrieval gains are not hidden by answer-side error.
7. **Production quality counter.** Ledger-derived signals (judge-edge contribution rate, abstain rate, fallback rate) sit next to the latency histograms in `kb_status(include_diagnostics)`.

## 6. Risks and rejected ideas

**Risks and mitigations:**

- **Jev is about six weeks old, with unverified calibration and retention.** It is off by default and sends Standard visibility only. It runs in shadow mode before acting, has an ECE gate, records a fingerprint per verdict, and re-runs its gate on every model bump. `LocalJudge` is the exit.
- **Correlated remote outages (Voyage and Jev).** Each feature has its own breaker and deadline, and everything degrades to today's behaviour.
- **Injection through stored memories.** Jev outputs only probabilities and outcomes are add-only, so the worst case is a skewed ranking. That shows up in the ledger.
- **Enrichment hallucination.** It is used for indexing only, tagged with `enrich_model`, and can be regenerated.
- **Feedback loops** (judge edges feed spreading, which feeds co-retrieval). Edges are capped, tagged `source=judge`, and the hub cap is respected.
- **Overfitting to LoCoMo.** Held-out split plus the synthetic bench as a guard.
- **Three circuit breakers.** Accepted: they are independent failure domains, documented in an ADR.

**Rejected ideas:**

| Idea | Reason |
|---|---|
| Jev stacked after Voyage on the hot path | Latency adds up (p95 about 2 s). Jev replaces Voyage, and Voyage becomes the fallback. |
| HyDE | Drifts on precise and numeric queries, and Jev cannot generate. |
| Second `enrich_vector`, `enrich_questions` | Unproven value, new schema and backfill cost. Revisit after P0.8 and P2.2. |
| Haiku or Sonnet for classification decisions | Generation used to classify, with uncalibrated self-reported confidence. Jev is cheaper and returns distributions. |
| Opus or Fable at runtime | Cost and latency. Their value is labels. |
| Judges that archive, retract or lower visibility | Memory-poisoning defence (arXiv 2605.08442). |
| Judge state built from summaries only | Fidelity loss. The real limit is 32 KB, so state uses a 600-char content prefix. |
| Per-recall reconsolidation spawns | DB write amplification on SurrealKV. Use the hourly batch instead. |
| PII screen auto-upgrading to Sensitive | Regex precision is too low and would silently remove memories from pipelines. It warns and tags instead. |
| Jev Box, `-latest` alias | Early or unstable features. |
| Gates on the 196-QA set | Inside the noise floor. |
| Keeping MMR λ=0.7 while it is unmeasured | It cannot recover truncated items and may displace the top hit. |

## 7. Open decisions for the owner

1. **Is Jev egress allowed at all?** TypeSafe's own retention policy text was not found, and the summaries come from third parties. My recommendation: yes for Standard visibility after you read their policy, same opt-in as ADR-016. If not, P1–P2 judges run on `SidecarJudge` (Claude structured output) at higher latency until `LocalJudge` exists.
2. **Recall latency budget.** p50 is 1167 ms today. Should R2 be held to "≤ Voyage's median", or may it add up to 200 ms for a Recall@1 gain?
3. **Voyage rerank after the A/B.** If Jev wins, drop the Voyage rerank subscription and keep it only as the fallback?
4. **Where `bench/` lives.** Tracked submodule or a separate repo, and whether the LoCoMo data licence allows it.
5. **Sidecar transport for Haiku.** Keep the Agent SDK, which may carry a large fixed prompt per call, or move T2 calls to the direct Messages API with prompt caching inside the sidecar? Measure per-call overhead in P0.4 and decide then.
6. **Re-embed window for P0.8.** A full `kb_reembed` against Voyage costs money and time. Approve it once P0.5–P0.7 are measured.
7. **Fate of `domain_decay_rate`.** Default: delete it in P3 unless the bench shows a decay effect.