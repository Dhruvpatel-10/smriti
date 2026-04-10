# Project Structure

```
smriti/
+-- CLAUDE.md                    <- compact project instructions (loaded every session)
+-- Cargo.toml
+-- config.toml                  <- server config (see docs/config.md)
+-- src/
|   +-- main.rs                  <- entry point: start MCP stdio server
|   +-- lib.rs                   <- module declarations
|   +-- server.rs                <- rmcp server setup, tool registration
|   +-- config.rs                <- config loading, validation
|   +-- error.rs                 <- thiserror error types
|   +-- types/
|   |   +-- mod.rs               <- re-exports all type modules
|   |   +-- enums.rs             <- MemoryType, MemorySubtype (23), Source, Visibility, RelationType, Importance
|   |   +-- ids.rs               <- MemoryId, EntityId newtypes
|   |   +-- memory.rs            <- Memory, MemorySummary, MemoryFull structs
|   |   +-- entity.rs            <- Entity, EntitySummary, EntityFull structs
|   |   +-- status.rs            <- StatusReport, QueueEntry structs
|   |   +-- alert.rs             <- Alert, AlertType
|   |   +-- export.rs            <- ExportResult, ImportResult structs
|   +-- db/
|   |   +-- mod.rs
|   |   +-- client.rs            <- SurrealDB connection, migration runner
|   |   +-- migrations.rs        <- numbered schema migrations (never edit existing)
|   |   +-- memory.rs            <- CRUD for memories
|   |   +-- entity.rs            <- CRUD for flexible entities
|   |   +-- graph.rs             <- RELATE operations (involves, assigned, updates, extends, derives, relates)
|   |   +-- search.rs            <- hybrid retrieval: BM25 + vector RRF + spreading activation (5 stages)
|   |   +-- embeddings.rs        <- LocalEmbedder: in-process ONNX via fastembed
|   |   +-- validation.rs        <- write-path intelligence: inference, importance, TTL, summary gate, contextual embedding
|   |   +-- dedup.rs             <- cosine similarity dedup (0.92 reject, 0.82 warn)
|   |   +-- auto_link.rs         <- embedding similarity (0.75/0.90) + whole-word entity matching
|   |   +-- prune.rs             <- TTL expiry, SM-2 decay with immunity, archive threshold
|   |   +-- alerts.rs            <- overdue items, stale contacts, expiring memories, contradictions
|   |   +-- export.rs            <- backup JSONL export/import
|   |   +-- markdown.rs          <- Layer 1 markdown generation (~/.claude/CLAUDE.md)
|   |   +-- consolidation.rs     <- consolidation candidate detection
|   |   +-- chunking.rs          <- content chunking for long documents + vector search
|   |   +-- reembed.rs           <- re-embedding migration (model/dim changes)
|   +-- tools/
|   |   +-- mod.rs
|   |   +-- remember.rs          <- kb_remember: validate→embed→dedup→create→post-create
|   |   +-- recall.rs            <- kb_recall handler
|   |   +-- recall_at.rs         <- kb_recall_at handler
|   |   +-- drill.rs             <- kb_drill handler
|   |   +-- forget.rs            <- kb_forget handler
|   |   +-- promote.rs           <- kb_promote handler
|   |   +-- queue.rs             <- kb_queue handler
|   |   +-- status.rs            <- kb_status handler
|   |   +-- link.rs              <- kb_link handler
|   |   +-- consolidate.rs       <- kb_consolidate handler
|   |   +-- entity.rs            <- kb_entity_* handlers
|   |   +-- export.rs            <- kb_export/kb_import handlers
|   |   +-- alerts.rs            <- kb_alerts handler
|   +-- llm/
|   |   +-- mod.rs               <- LLM module
|   |   +-- client.rs            <- Gemini primary + Groq fallback (from env vars)
|   |   +-- classify.rs          <- LLM-based memory reclassification
+-- tests/                       <- 177 tests (70 unit + 107 integration)
|   +-- common/mod.rs            <- shared helpers: test_db(), create_test_memory(), remember_input()
|   +-- memory_crud.rs           <- 17 tests: CRUD, TTL, promote, schema idempotency
|   +-- search_retrieval.rs      <- 17 tests: recall, filters, vector, RRF, spreading activation
|   +-- graph_and_entities.rs    <- 10 tests: edges, entity CRUD, idempotency
|   +-- pruning_and_decay.rs     <-  9 tests: TTL expiry, SM-2 decay, immunity, legacy fallback
|   +-- alerts_and_status.rs     <-  8 tests: overdue, expiring, contradictions, status
|   +-- tool_tests.rs            <- 35 tests: all MCP tool handlers
|   +-- queue_tests.rs           <-  8 tests: queue file I/O
|   +-- consolidation.rs         <-  1 test: merge + derives provenance
|   +-- export_import.rs         <-  1 test: JSONL roundtrip
|   +-- migrations.rs            <-  1 test: schema version tracking
+-- docs/
|   +-- architecture.md          <- two-layer storage, server intelligence, write/retrieval pipelines
|   +-- data-model.md            <- entities, graph, visibility, decay, importance
|   +-- config.md                <- config.toml reference + env vars + MCP connection
|   +-- surrealdb-v3.md          <- SurrealDB v3 gotchas and dev notes
|   +-- project-structure.md     <- this file
|   +-- decisions/               <- Architecture Decision Records (ADR-001 through ADR-008)
+-- data/                        <- SurrealDB SurrealKV files (gitignored)
+-- queue/                       <- instruction queue (gitignored)
+-- archive/                     <- raw source material + exports (gitignored)
+-- scripts/                     <- drain.sh, backup.sh, install.sh, systemd units
```
