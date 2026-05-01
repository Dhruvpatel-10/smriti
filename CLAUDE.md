# Smriti — Persistent Second Memory

Second memory. Local-first Rust MCP server, SurrealDB-backed — everything learned, decided, experienced, recallable across session, project, device.
Local default; opt-in cloud embedder (Voyage AI, paid-tier privacy gate) for asymmetric query/document retrieval quality (ADR-016).

**Detailed docs in `docs/` — read for architecture, data model, SurrealDB notes.**

---

## Taxonomy (5 types, 23 subtypes)

Type + subtype REQUIRED in `kb_remember` — caller classify. Server validates pairing.

```
episodic (6):   meeting, conversation, daily_log, reflection, session, research
semantic (6):   decision, fact, preference, value, metric, pattern
procedural (2): workflow, skill
prospective (4): action_item, goal, life_goal, experiment
working (5):    priority (7d), open_question (30d), scratch (48h), signal (14d), hypothesis (7d)
```

Importance floors: decision≥4, value=5, life_goal=5, action_item≥3, goal≥4.
Ceilings: scratch≤2, daily_log≤2.

---

## MCP Tool Signatures

```
kb_remember(summary, type, subtype, channel?, modality?, content?, project?, domain?,
            people?, importance?, visibility?, event_at?, tags?, updates_id?,
            source_agent?, is_static?) -> {id, message}
            // source_agent must match a registered active agent name — archived agents rejected
            // agent-authored working-subtype memories default to visibility=Private

kb_recall(query, type?, project?, domain?, person?, since?, until?, author_filter?, limit?, rerank?) -> [{id, summary, ...}]
kb_recall_at(query, as_of, project?, domain?, author_filter?, limit?) -> [{id, summary, ...}]
kb_drill(id) -> full memory + provenance
kb_archive(id, reason, correction_id?) -> {message}
            // reason=forget: "no longer relevant" → invalidation_reason=forgotten
            // reason=retract: "I was wrong" → invalidation_reason=retracted; correction_id creates retracts edge
kb_promote(id) -> ()           // clear TTL, bump importance to ≥3

kb_entity_upsert(name, kind, roles?, attrs?, context?) -> EntityId
kb_entity_find(query, kind?, role?) -> [EntitySummary]  // use kind=person to find people
kb_entity_get(id) -> EntityFull

kb_agent_register(name, kind, model?, version?, parent?, project?, config?) -> {id, message}
kb_agent_retire(id_or_name, purge_memories?=false) -> {message}
kb_agent_query(id_or_name?, project?, include_archived?) -> {single?, list}
            // with id_or_name: returns AgentFull in single field; list is empty
            // without id_or_name: returns AgentSummary list; single is null

kb_link(from_id, to_id, relation) -> ()    // updates|extends|derives|relates
kb_relation_retract(from, to, kind, reason?) -> {message, retracted}   // soft-retract typed_relation edge; idempotent
kb_entity_delete(id, reason?) -> {message, deleted}                    // soft-archive entity; archived_at tombstone, hidden from find/get
kb_consolidate(ids, result_summary) -> MemoryId
kb_queue(instruction, source_project) -> ()
kb_status(project?, domain?) -> StatusReport
kb_alerts() -> [Alert]
kb_export(format?) -> ExportResult
kb_import(path) -> ImportResult
kb_regenerate_md() -> ()
kb_consolidation_candidates(limit?) -> [ConsolidationCandidate]
kb_reembed() -> ReembedResult
```

---

## Write Path (Embed-First, Create-Last)

0. **Agent resolve** — `source_agent` name → `AgentId` (archived rejected, project inherited, working subtypes default Private)
1. **Validate** — summary 10-500 chars, validate type/subtype pairing, importance bounds, TTL
2. **Embed** — contextual: `[type/subtype] [project] [domains] summary`
3. **Dedup** — cosine >0.92 reject, >0.82 warn (pre-create, no orphans; scoped per-author — same-author bucket only)
4. **Create** — write to DB
5. **Post-create (async)** — store vector, link entities (sets `last_contact`), assigned edge, auto-link, entity name match, chunk long content, touch `agent.last_active`; extract typed relations (MIGRATION_012) + anchor entities (MIGRATION_014) via sidecar — both concurrent, both skip `Visibility::Sensitive`

Archive/supersede/retract paths set `invalidated_at` + `invalidation_reason` + `status='archived'`. Invariant: `status='archived'` iff `invalidated_at IS NOT NULL`.

## Retrieval (5+ stages)

BM25 → Vector → Fusion (RRF default | Convex Combination, configurable) → Adaptive Rerank (ADR-022 policy, optional caller override) → Entity-anchored person filter → Weighted Spreading Activation (per-edge-type, ADR-023, configurable cap)

---

## Code Rules

- `MemoryType`/`MemorySubtype` enums — never raw strings
- `MemoryId`/`EntityId` newtypes — no raw string IDs
- `AgentId` newtype — no raw string agent IDs; `AgentKind`/`AgentStatus` enums
- All SurrealQL uses parameterized bindings — no string interpolation
- Visibility enforced in ALL query paths
- Never log memory content (privacy) — only IDs + types
- Never hard-delete — archived memories stay in DB, `BITEMPORAL_ACTIVE` predicate (`invalidated_at IS NONE AND (valid_until IS NONE OR valid_until > time::now())`) filters reads; `status='archived'` kept as derived index hint
- `InvalidationReason` enum — never raw strings in Rust; DB stores as lowercase snake_case
- Single TTL source: `validation::ttl_duration()` (not memory.rs)
- `thiserror` in library, MCP error responses in tool handlers, never panic
- Log to stderr (stdout reserved for MCP protocol)
- Recall paths use `embed_query()`, write/store paths use `embed_document()` — Voyage backend exploits asymmetric `input_type`; local backend ignores
- Embedder fingerprint (`provider/model/dim`) gated at startup against `_kb_metadata` (Migration 015) — mismatch with stored vectors → refuse to start, run `kb_reembed`

---

## Key Constraints

- Architecture decided — no re-debate (see `docs/architecture.md`)
- Drain session OPTIONAL for correctness — system works without
- Server enforces quality regardless of calling LLM smartness
- Big test suite; 2 ignored: voyage integration (key-gated); 5 ignored sidecar integration (live API required). 0 clippy warnings — keep so.
- `Embedder` trait (`src/db/embeddings/traits.rs`), not enum — `Arc<dyn Embedder>` everywhere; `build_embedder(cfg)` factory returns concrete impl
- `SMRITI_FORCE_REEMBED_ON_START=1` operator escape for fingerprint mismatch on model swap — must run `kb_reembed` immediately after via `smriti-librarian` subagent (global write-path protocol); prevents systemd crash loops. Full runbook: `docs/operations/embedder-swap.md`
- Auto-consolidation runs as `auto-consolidator` agent (kind: `Summarizer`) when `[consolidation].auto_enabled = true`. Default: off. Uses second isolated `SidecarClient` — circuit-breaker independent from post-create extraction. See ADR-026.