# ADR-001: Embedded SurrealDB with SurrealKV

**Status:** Accepted  
**Date:** 2026-04-08  
**Context:** Need a database for a local-first, privacy-preserving personal knowledge base. Requirements: graph relations, full-text search, no cloud dependency, zero ongoing cost, embeddable in a Rust binary.

## Decision

Use SurrealDB v3 with the `kv-surrealkv` embedded engine (no daemon).

## Rationale

- **Graph-native:** RELATE edges for memory→entity, memory→memory relations without a separate graph DB.
- **Embedded:** SurrealKV runs in-process. No daemon to manage, no port conflicts, no network hop.
- **FTS built-in:** `DEFINE ANALYZER ... BM25` + `@@` operator. No ElasticSearch/Meilisearch dependency.
- **Schema flexibility:** SCHEMAFULL tables with `TYPE object FLEXIBLE` for freeform entity attrs.
- **Single binary:** Ships as one Rust binary with DB included.

## Tradeoffs

- **File locking:** SurrealKV locks data files — only one process can open them. Solved via HTTP daemon transport (ADR-002).
- **No MTREE ANN index:** The embedded engine doesn't support vector indexes yet. Brute-force cosine similarity is fast enough for <100k records.
- **SurrealQL quirks:** `type::record()` for SELECT positions, `RecordId::new()` for RELATE. `IF/ELSE` uses `{}` not `THEN/END`. `.take()` required to surface embedded errors.

## Consequences

- All data stays on machine. Zero cloud dependency.
- Schema managed via numbered migrations (ADR-003).
- Vector search uses brute-force scan until SurrealKV supports MTREE.
