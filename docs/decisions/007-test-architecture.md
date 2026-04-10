# ADR-007: Modular Test Architecture

**Status:** Accepted  
**Date:** 2026-04-10  
**Context:** Tests were in two monolithic files (db_tests.rs: 1600 lines, tool_tests.rs: 1000 lines). Hard to find tests, hard to run a focused subset, hard to maintain.

## Decision

Split into focused test modules, one per feature area, with shared helpers.

## Structure

```
tests/
  common/mod.rs           — test_db(), create_test_memory(), remember_input()
  memory_crud.rs          — 17 tests: CRUD, TTL, promote, schema idempotency
  search_retrieval.rs     — 17 tests: recall, filters, RRF, vector, spreading activation, ordering
  graph_and_entities.rs   — 10 tests: edges, entity CRUD, idempotency
  pruning_and_decay.rs    —  9 tests: TTL expiry, SM-2 decay, legacy fallback
  alerts_and_status.rs    —  8 tests: overdue, expiring, contradictions, status
  tool_tests.rs           — 35 tests: all MCP tool handlers
  queue_tests.rs          —  8 tests: queue file I/O
  consolidation.rs        —  1 test:  merge + derives provenance
  export_import.rs        —  1 test:  JSONL roundtrip
  migrations.rs           —  1 test:  schema version tracking
```

## Principles

- **One file per concern:** easy to find, easy to run (`cargo test --test pruning_and_decay`).
- **Shared helpers in common/:** `test_db()` spins up a fresh DB; `create_test_memory()` and `remember_input()` reduce boilerplate.
- **No helper duplication:** helpers live ONLY in `common/mod.rs`, not copied per file. Prevents the dead-code warning issue from having unused helpers in specific test binaries.
- **DB-layer vs tool-layer separation:** `*_tests.rs` for DB layer, `tool_tests.rs` for MCP tool handlers. Tool tests verify the full stack; DB tests verify individual operations.

## Consequences

- 145 total tests (37 unit + 108 integration), all passing.
- Each test file compiles independently — parallel test execution.
- Adding tests for a new feature = add a new file or append to the relevant module.
