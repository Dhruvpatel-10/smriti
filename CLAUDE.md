# Smriti — Persistent Second Memory

Your second memory. Local-first Rust MCP server backed by SurrealDB — everything you've
learned, decided, and experienced, recallable across any session, project, or device.
Zero cloud dependency.

**Detailed docs in `docs/` — read those for architecture, data model, and SurrealDB notes.**

---

## Taxonomy (5 types, 23 subtypes)

Type and subtype are REQUIRED in `kb_remember` — caller must classify. Server validates the pairing.

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
kb_remember(summary, source, type, subtype, content?, project?, domain?,
            people?, importance?, visibility?, event_at?, tags?, updates_id?,
            source_agent?) -> {id, message}

kb_recall(query, type?, project?, domain?, person?, since?, until?, limit?) -> [{id, summary, ...}]
kb_recall_at(query, as_of, project?, domain?, limit?) -> [{id, summary, ...}]
kb_drill(id) -> full memory + provenance
kb_forget(id) -> ()           // soft archive, never hard-delete
kb_promote(id) -> ()           // clear TTL, bump importance to ≥3

kb_entity_upsert(name, kind, roles?, attrs?, context?) -> EntityId
kb_entity_find(query, kind?, role?) -> [EntitySummary]
kb_entity_get(id) -> EntityFull
kb_people(name) -> [EntitySummary]

kb_link(from_id, to_id, relation) -> ()    // updates|extends|derives|relates
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

1. **Validate** — summary 10-500 chars, validate type/subtype pairing, importance bounds, TTL
2. **Embed** — contextual: `[type/subtype] [project] [domains] summary`
3. **Dedup** — cosine >0.92 reject, >0.82 warn (pre-create, no orphans)
4. **Create** — write to DB
5. **Post-create (async)** — store vector, link entities (sets `last_contact`), assigned edge, auto-link, entity name match, chunk long content

## Retrieval (5 stages)

BM25 → Vector → RRF fusion (always balanced) → Entity-anchored person filter → Spreading activation (cap 3)

---

## Code Rules

- `MemoryType`/`MemorySubtype` are enums — never raw strings
- `MemoryId`/`EntityId` are newtypes — no raw string IDs
- All SurrealQL uses parameterized bindings — no string interpolation
- Visibility enforced in ALL query paths
- Never log memory content (privacy) — only IDs and types
- Never hard-delete — archived memories stay in DB, `WHERE status = 'active'` filters
- Single TTL source: `validation::ttl_duration()` (not memory.rs)
- `thiserror` in library, MCP error responses in tool handlers, never panic
- Log to stderr (stdout reserved for MCP protocol)

---

## Key Constraints

- Architecture is decided — don't re-debate (see `docs/architecture.md`)
- Drain session is OPTIONAL for correctness — system works without it
- Server enforces quality regardless of how smart the calling LLM is
- 168 tests, 0 clippy warnings — keep it that way
