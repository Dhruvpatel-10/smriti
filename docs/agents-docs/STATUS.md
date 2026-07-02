# smriti audit status — dashboard

Last aligned: 2026-06-06 (ADR-031 MMR diversification shipped; bench delta pending). Prior: 2026-05-07 (LoCoMo bench delta after **reranker default-on flip**).

## Last bench (LoCoMo10, 2026-05-07 — reranker default-on)

| Metric | Baseline (05-07 no rerank) | Current (05-07 +rerank) | Delta |
|---|---|---|---|
| Overall answer accuracy | 61.4% (89/145) | **63.4%** (92/145) | +2.0 pts |
| Retrieval hit @10 (judge view) | 55.2% (80/145) | 55.9% (81/145) | +0.7 pts |
| Conditional acc (correct when hit) | 76.2% (61/80) | 76.5% (62/81) | flat (+0.3 pts) |
| Recall@1 (oracle) | 2.6% (5/196) | **12.2%** (24/196) | **+9.6 pts, 4.7×** |
| Recall@5 (oracle) | 27.6% (54/196) | **44.9%** (88/196) | **+17.3 pts, 1.6×** |
| Recall@10 (oracle) | 49.5% (97/196) | 50.0% (98/196) | flat |
| Recall p50 | 853 ms | 1167 ms | +314 ms (rerank RTT) |
| Recall p95 | 921 ms | 1333 ms | +412 ms |
| Ingest p95 | 396 ms | 430 ms | +34 ms |
| DB on-disk | 281 MB | 281 MB | flat |

- Voyage voyage-context-3 (1024 dims), samples=1, K=10, **rerank-2.5-lite ON (adaptive policy)**
- 145 probes graded, 196 oracle QAs, parity with 2026-05-07 baseline
- **Reranker default flipped** to `enabled=true` (ADR-022 promoted Proposed→Accepted)
- Top-1 hits 5 → 24 (4.7×) — rerank pulls rank 2-4 hits up. Hit@10 flat = doesn't recall new memories
- Latency tax ~310ms p50 from Voyage rerank API call
- Cost: ~$0.11 total (Voyage embeds + rerank), $0 judge (Claude Code Max plan)
- Files: `bench/results/{retrieval,judge,probes,judge.jsonl}.md`, prior baseline backed up to `bench/results/_archive_20260507_pre_rerank/`

### Open levers (next bench cycle)

- Answer accuracy stuck ~63% — bottleneck moved from ranking to **retrieval miss rate (44%)**. Reranker is hit@10-bound; need lever that adds new hits.
- **MMR diversity post-fusion** — SHIPPED (ADR-031, default-on, λ=0.7). Quality delta **pending next bench cycle** — shipped on diagnosis, not yet LoCoMo-measured. If regression, flip `[search.mmr].enabled=false` (one-line, no code revert).
- **HyDE for entity+topic queries** — bigger swing on profile/list, ~$0.001/query
- **Larger candidate K (30 → rerank to 10)** — more rerank pool, catch hits ranked 11-30; stacks on MMR (bigger pool = more clusters to diversify)
- Alias seeding pending → ADR-030 unmeasured
- Synth bench oracle translator broken — `expected_translated_ids: []` empty

## Shipped decisions

| ADR | Description | Date | Tests |
|---|---|---|---|
| ADR-009 | v0.3.3 security & concurrency hardening | 2026-04-21 | — |
| ADR-010 | Agent as first-class authorship model | 2026-04-21 | — |
| ADR-011 | Bitemporal memory model Phase 1 (migrations 007+008, kb_retract, BITEMPORAL_ACTIVE predicate, recall_at correctness fix) | 2026-04-21 | 16 new tests in tests/bitemporal.rs; total 314 |
| ADR-013 | Claude Agent SDK sidecar for typed-relation extraction (MIGRATION_012, typed_relation edges, spreading activation integration) | 2026-04-29 | — |
| ADR-014 | Write-time anchor entity extraction Phase 2-B (MIGRATION_014, anchor_entities field + BM25 index, extract_anchors sidecar method, --reextract-anchors CLI) | 2026-04-30 | 339 → 352 tests |
| ADR-026 | Server-side auto-consolidation loop; synthesize_consolidation sidecar method; consolidation_attempt ledger (migration 018); per-author scoping; isolated SidecarClient; auto_enabled=false default | 2026-04-30 | new ledger + auto-consolidation unit tests + heuristic-2 un-ignored |
| ADR-022 | Adaptive reranker policy + default-on flip (`enabled=true`); Voyage rerank-2.5-lite, 4.7× Recall@1 lift, +310ms p50 tax | 2026-05-07 | 631 (629 + 2 new VoyageRerankerConfig default tests) |
| ADR-031 | MMR post-fusion diversification (stage 3.4, default-on, λ=0.7); `[search.mmr]` config, `RecallStage::Mmr` metric; targets cluster-flooding on aggregation/list queries. Quality delta pending next bench | 2026-06-06 | +3 (mmr_skips_when_disabled, mmr_lambda_one_equals_relevance_only, mmr_diversifies_clustered_pool) |

## Open findings (3)

| # | Severity | Category | Location | One-line |
|---|----------|----------|----------|----------|
| 1 | MEDIUM   | memory-performance     | src/db/embeddings/store.rs | Unnecessary `.to_vec()` clone on vector binding (perf only, not correctness) |
| 2 | LOW      | memory-performance     | src/db/prune.rs:52,120,175 | `RETURN AFTER` fetches full rows when count would suffice |
| 3 | LOW      | type-safety-modularity | src/server.rs `kb_consolidation_candidates` | `u8` limit conversion — now uses `usize::from()`, harmless |

All CRITICAL and HIGH findings across all 8 categories are CLOSED.

## Per-category status

| Category | File | Open | Last audit |
|----------|------|------|------------|
| Security          | security-audit.md              | 0 (4 INFO + 3 auto-consolidation INFO remaining) | 2026-04-30 |
| Concurrency       | concurrency-safety.md          | 0 | 2026-04-21 |
| Error handling    | error-handling-audit.md        | 0 | 2026-04-20 |
| Logging           | logging-observability-audit.md | 0 | 2026-04-20 |
| Memory / perf     | memory-performance.md          | 2 | 2026-04-20 |
| SurrealDB queries | surrealdb-query-audit.md       | 0 | 2026-04-30 |
| Test coverage     | test-coverage-gaps.md          | 9 open gaps (2 MEDIUM, 7 LOW); 3 new closed by ADR-026 | 2026-04-30 |
| Type safety       | type-safety-modularity.md      | 1 | 2026-04-20 |

## How to use

- Auditors: read the rubric at the top of the relevant per-category file, then re-verify only the Open findings listed above.
- Orchestrator: this file is the single-source-of-truth for "what's left". Per-category files are the audit trail + rubric reference.
- After closing a finding: update this table AND the per-category file.
