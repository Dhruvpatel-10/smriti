# smriti audit status — dashboard

Last aligned: 2026-04-30 (ADR-026 auto-consolidation shipped; sidecar extended with synthesize_consolidation).

## Shipped decisions

| ADR | Description | Date | Tests |
|---|---|---|---|
| ADR-009 | v0.3.3 security & concurrency hardening | 2026-04-21 | — |
| ADR-010 | Agent as first-class authorship model | 2026-04-21 | — |
| ADR-011 | Bitemporal memory model Phase 1 (migrations 007+008, kb_retract, BITEMPORAL_ACTIVE predicate, recall_at correctness fix) | 2026-04-21 | 16 new tests in tests/bitemporal.rs; total 314 |
| ADR-013 | Claude Agent SDK sidecar for typed-relation extraction (MIGRATION_012, typed_relation edges, spreading activation integration) | 2026-04-29 | — |
| ADR-014 | Write-time anchor entity extraction Phase 2-B (MIGRATION_014, anchor_entities field + BM25 index, extract_anchors sidecar method, --reextract-anchors CLI) | 2026-04-30 | 339 → 352 tests |
| ADR-026 | Server-side auto-consolidation loop; synthesize_consolidation sidecar method; consolidation_attempt ledger (migration 018); per-author scoping; isolated SidecarClient; auto_enabled=false default | 2026-04-30 | new ledger + auto-consolidation unit tests + heuristic-2 un-ignored |

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
