# Architecture Decision Records

Decisions that shaped this system. Format: [ADR](https://adr.github.io/).

| # | Decision | Date | Status |
|---|----------|------|--------|
| [001](001-embedded-surrealdb.md) | Embedded SurrealDB with SurrealKV | 2026-04-08 | Accepted |
| [002](002-http-daemon-transport.md) | HTTP daemon transport for multi-session | 2026-04-08 | Accepted |
| [003](003-schema-migrations.md) | Numbered schema migration system | 2026-04-08 | Accepted |
| [004](004-embedding-model.md) | EmbeddingGemma-300M via fastembed | 2026-04-10 | Accepted |
| [005](005-hybrid-retrieval.md) | Hybrid retrieval: RRF + spreading activation + Lost-in-Middle | 2026-04-10 | Accepted |
| [006](006-sm2-forgetting-curve.md) | SM-2 spaced repetition for memory decay | 2026-04-10 | Accepted |
| [007](007-test-architecture.md) | Modular test architecture | 2026-04-10 | Accepted |
| [008](008-llm-integration-and-server-intelligence.md) | LLM integration, server-side intelligence, and taxonomy refinement | 2026-04-10 | Accepted |
| [009](009-v0-3-3-hardening.md) | v0.3.3 security & concurrency hardening | 2026-04-21 | Accepted |
| [010](010-agent-authorship.md) | Agent as first-class authorship model | 2026-04-21 | Accepted |
| [011](011-bitemporal-memory.md) | Bitemporal memory model (Phase 1) | 2026-04-21 | Accepted |
| [013](013-claude-sdk-sidecar.md) | Claude Agent SDK sidecar (TS) for typed-relation extraction; Max subscription auth; subprocess isolation | 2026-04-29 | Accepted |
| [014](014-write-time-anchor-extraction.md) | Write-time anchor entity extraction (Phase 2-B); BM25 index on anchor_entities; zero recall-time cost | 2026-04-30 | Accepted |
| [015](015-embedder-swap.md) | Swap bench embedder to GTE-Large-EN-V1.5-Q (int8); same 1024-dim, +1.2 MTEB, 8x RAM reduction | 2026-04-30 | Accepted |
| [016](016-cloud-embedder-backend.md) | Voyage AI cloud embedder backend; asymmetric `input_type=query|document`; paid-tier privacy gate; embedder fingerprint (Migration 015) at startup | 2026-04-30 | Accepted |
| [017](017-voyage-reranker.md) | Voyage `rerank-2.5-lite` cross-encoder reranker at stage 3.5 (after RRF, before entity-filter); opt-in; soft-fail on outage | 2026-04-30 | Accepted |
| [018](018-voyage-batch-bench-ingest.md) | Voyage Batch API (`/v1/files` + `/v1/batches`) for bench ingest; 33% discount; 12h SLA; production write path unchanged | 2026-04-30 | Accepted (bench-only) |
| [019](019-channel-modality-split.md) | Split `Source` enum into optional `Channel` + `Modality`; embed both in contextual text; source-aware visibility defaults; deterministic backfill migration | 2026-04-30 | Accepted |
| [020](020-chunking-architecture.md) | Chunking architecture — voyage-context-3 contextualised chunk embeddings; recursive structural splitter (800-tok Voyage / 350-tok local); reranker payload fix; `kb_reembed` chunk-aware; TTL cascade to `extends`-children; UTF-8 safety | 2026-04-30 | Accepted |
| [021](021-graded-relevance-oracle.md) | Extend oracle schema with optional `graded_relevance: HashMap<MemoryId, u8>` (0–3); additive; new eval runner emits nDCG@10, MRR@10, Recall@{5,10,20} alongside binary hit/miss; 100–200 hand-labeled probes, never bootstrapped from pipeline output | 2026-04-30 | Proposed |
| [022](022-adaptive-reranker-policy.md) | `RerankerPolicy` enum (`Always`/`Adaptive`/`Never`); per-query skip/force heuristics (RRF dominance, query length, memory type, person filter); optional `rerank` param on `kb_recall`; `rerank_decision_total` metrics; thresholds calibrated against ADR-021 | 2026-04-30 | Proposed |
| [023](023-weighted-spreading-activation.md) | Per-edge-type weight coefficients in spreading activation: `derives=1.0`, `extends/updates=0.8`, `relates=0.5`, `involves=0.3`, `typed_relation=confidence×0.9`; `hop_decay=0.5`; cap configurable via `[search.spreading]`; ~50 LOC, no schema change | 2026-04-30 | Proposed |
| [024](024-mcp-tool-surface-consolidation.md) | Drop `kb_people`; merge `kb_forget`+`kb_retract` → `kb_archive(id, reason: ArchiveReason, correction_id?)`; merge `kb_agent_list`+`kb_agent_get` → `kb_agent_query`; net 28 → 25 tools; no legacy aliases; backward-incompatible | 2026-04-30 | Proposed |
| [025](025-smriti-librarian-subagent.md) | Hybrid read/write split: `~/.claude/agents/smriti-librarian.md` subagent handles all writes (~16 tools); main session reads directly (~10 tools); write latency +1–2 s, cost +$0.003/write; `Visibility::Sensitive` privacy filter on librarian boundary | 2026-04-30 | Proposed |
| [026](026-auto-consolidation.md) | Server-side periodic auto-consolidation loop; sidecar `synthesize_consolidation`; `consolidation_attempt` ledger (migration 018); per-author scoping; isolated `SidecarClient`; `auto_enabled = false` by default | 2026-04-30 | Proposed |
