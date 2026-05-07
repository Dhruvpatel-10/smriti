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
|   |   +-- embeddings/          <- Embedder trait + backends
|   |   |   +-- mod.rs           <- re-exports + factory
|   |   |   +-- traits.rs        <- Embedder trait, EmbedderFingerprint
|   |   |   +-- factory.rs       <- config → Arc<dyn Embedder>
|   |   |   +-- fingerprint.rs   <- _kb_metadata startup gate; SMRITI_FORCE_REEMBED_ON_START
|   |   |   +-- local.rs         <- LocalEmbedder (fastembed ONNX, 384-dim)
|   |   |   +-- store.rs         <- store_vector helper
|   |   |   +-- voyage/          <- VoyageEmbedder (voyage-context-3, 1024-dim)
|   |   |       +-- mod.rs
|   |   |       +-- client.rs
|   |   |       +-- contextualized.rs  <- /v1/contextualizedembeddings
|   |   |       +-- wire.rs            <- request/response types
|   |   +-- validation.rs        <- write-path intelligence: inference, importance, TTL, summary gate, contextual embedding
|   |   +-- dedup.rs             <- cosine similarity dedup (0.92 reject, 0.82 warn)
|   |   +-- auto_link.rs         <- embedding similarity (0.75/0.90) + whole-word entity matching
|   |   +-- prune.rs             <- TTL expiry, SM-2 decay with immunity, archive threshold
|   |   +-- alerts.rs            <- overdue items, stale contacts, expiring memories, contradictions
|   |   +-- export.rs            <- backup JSONL export/import
|   |   +-- markdown.rs          <- Layer 1 markdown generation (~/.claude/CLAUDE.md)
|   |   +-- consolidation.rs     <- consolidation candidate detection + consolidate_atomic
|   |   +-- consolidation_ledger.rs <- auto-consolidation attempt ledger (migration 018, ADR-026)
|   |   +-- chunking.rs          <- content chunking for long documents + vector search
|   |   +-- reembed.rs           <- re-embedding migration (model/dim changes)
|   |   +-- recall_metrics.rs    <- hdrhistogram per-stage latency (BM25/Vector/Fusion/Rerank/SpreadingActivation); p50/p95/p99/max µs; snapshot_and_reset for kb_status (ADR-028)
|   +-- tools/
|   |   +-- mod.rs
|   |   +-- remember.rs          <- kb_remember: validate→embed→dedup→create→post-create
|   |   +-- recall.rs            <- kb_recall handler
|   |   +-- recall_at.rs         <- kb_recall_at handler
|   |   +-- drill.rs             <- kb_drill handler
|   |   +-- archive.rs           <- kb_archive handler (reason=forget|retract; replaces kb_forget+kb_retract)
|   |   +-- promote.rs           <- kb_promote handler
|   |   +-- queue.rs             <- kb_queue handler
|   |   +-- status.rs            <- kb_status handler
|   |   +-- profile.rs           <- kb_profile handler
|   |   +-- link.rs              <- kb_link handler
|   |   +-- consolidate.rs       <- kb_consolidate handler
|   |   +-- entity.rs            <- kb_entity_upsert/find/get handlers
|   |   +-- entity_delete.rs     <- kb_entity_delete handler
|   |   +-- relation_retract.rs  <- kb_relation_retract handler
|   |   +-- export.rs            <- kb_export/kb_import handlers
|   |   +-- alerts.rs            <- kb_alerts handler
|   |   +-- agent.rs             <- kb_agent_register/retire/query handlers
|   +-- llm/
|   |   +-- mod.rs               <- LLM module; re-exports extract_and_persist, SidecarClient, ExtractStats
|   |   +-- sidecar.rs           <- SidecarClient; supervisor loop; circuit breaker; JSON-RPC 2.0; CircuitState + last_success_age; RpcMeta request_id envelope (ADR-028)
|   |   +-- extractor.rs         <- extract_and_persist + extract_anchors_and_persist; validates + writes typed_relation / anchor_entities
|   +-- observability/
|   |   +-- mod.rs               <- startup Instant via OnceLock; init() + uptime() -> Option<Duration> (ADR-028)
|   |   +-- throttle.rs          <- ThrottleLayer: per-callsite drop within configurable window; sidecar/llm allowlist; crash-class bypass; suppressed counter (ADR-028)
+-- tests/                       <- extensive test suite (2 ignored: voyage integration, key-gated; 5 ignored sidecar integration)
|   +-- common/mod.rs            <- shared helpers: test_db(), create_test_memory(), remember_input()
|   +-- memory_crud.rs           <- CRUD, TTL, promote, schema idempotency
|   +-- search_retrieval.rs      <- recall, filters, vector, RRF, spreading activation
|   +-- graph_and_entities.rs    <- edges, entity CRUD, idempotency
|   +-- pruning_and_decay.rs     <- TTL expiry, SM-2 decay, immunity, legacy fallback
|   +-- alerts_and_status.rs     <- overdue, expiring, contradictions, status
|   +-- tool_tests.rs            <- MCP tool handlers
|   +-- consolidation.rs         <- merge + derives provenance; consolidation_ledger; auto-consolidation unit tests
|   +-- export_import.rs         <- JSONL roundtrip
|   +-- migrations.rs            <- schema version tracking
+-- docs/
|   +-- architecture.md          <- two-layer storage, server intelligence, write/retrieval pipelines
|   +-- data-model.md            <- entities, graph, visibility, decay, importance
|   +-- config.md                <- config.toml reference + env vars + MCP connection
|   +-- surrealdb-v3.md          <- SurrealDB v3 gotchas and dev notes
|   +-- project-structure.md     <- this file
|   +-- decisions/               <- Architecture Decision Records (ADR-001 through ADR-028)
+-- data/                        <- SurrealDB SurrealKV files (gitignored)
+-- queue/                       <- instruction queue (gitignored)
+-- archive/                     <- raw source material + exports (gitignored)
+-- scripts/                     <- drain.sh, backup.sh, install.sh, systemd units
```
