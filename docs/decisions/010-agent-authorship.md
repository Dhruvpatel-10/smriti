# ADR-010: Agent as First-Class Authorship Model

**Status:** Accepted  
**Date:** 2026-04-21  
**Context:** User plans to build and retire LLM agents over time (code-reviewer, daily-summarizer, research-assistant, etc.). Current `source_agent: Option<String>` is write-only audit metadata — never queried, never validated, never filterable. No way to answer "what did my code-reviewer agent decide" or "stop showing memories from retired-agent-v1." Need first-class authorship without breaking the single-user assumption.

---

## Decision

Add an `agent` table as a peer to `memory` and `entity`. Memory gets `author: Option<AgentId>` (None = user/me, Some = agent). Retirement is soft-archive (`status=archived`), never hard-delete. Authorship is a cheap indexed field predicate, not a graph traversal.

---

## Why Not EntityKind::Agent

Adding agents as an `EntityKind` variant makes every "me-only" recall query pay graph traversal cost. The hottest predicate — "exclude agent-authored memories" — becomes a join instead of a field check. Subject-vs-writer semantics also collide: an entity row represents something the memory is *about*; an agent row represents something that *wrote* the memory. These are orthogonal axes and should not share a table.

## Why Not a Unified Principal/Actor Table

Overkill for single-user. Adds an abstraction layer (user-as-row, ACL machinery) without unlocking anything at current scale. Single-user invariant is preserved; a future `owner_user_id` column is the upgrade path when needed.

---

## Schema (Migration 006)

```sql
DEFINE TABLE agent SCHEMAFULL;

DEFINE FIELD id           ON agent TYPE string;
DEFINE FIELD name         ON agent TYPE string;
DEFINE FIELD kind         ON agent TYPE string;  -- assistant|reviewer|summarizer|researcher|custom
DEFINE FIELD model        ON agent TYPE option<string>;
DEFINE FIELD version      ON agent TYPE option<string>;
DEFINE FIELD parent_agent ON agent TYPE option<record<agent>>;
DEFINE FIELD project      ON agent TYPE option<string>;
DEFINE FIELD config       ON agent TYPE option<object>;
DEFINE FIELD status       ON agent TYPE string DEFAULT 'active';  -- active|archived
DEFINE FIELD spawned_at   ON agent TYPE datetime DEFAULT time::now();
DEFINE FIELD retired_at   ON agent TYPE option<datetime>;
DEFINE FIELD last_active  ON agent TYPE datetime DEFAULT time::now();

DEFINE INDEX idx_agent_name    ON agent FIELDS name UNIQUE;
DEFINE INDEX idx_agent_status  ON agent FIELDS status;
DEFINE INDEX idx_agent_project ON agent FIELDS project;

DEFINE FIELD author ON memory TYPE option<record<agent>>;
DEFINE INDEX idx_memory_author ON memory FIELDS author;
```

---

## Rust Types

```rust
// src/types/agent.rs
pub struct AgentId(String);  // newtype — no raw string IDs per CLAUDE.md

pub enum AgentKind {
    Assistant,
    Reviewer,
    Summarizer,
    Researcher,
    Custom,
}

pub struct Agent {
    pub id:           AgentId,
    pub name:         String,
    pub kind:         AgentKind,
    pub model:        Option<String>,
    pub version:      Option<String>,
    pub parent_agent: Option<AgentId>,
    pub project:      Option<String>,
    pub config:       Option<serde_json::Value>,
    pub status:       EntityStatus,
    pub spawned_at:   DateTime<Utc>,
    pub retired_at:   Option<DateTime<Utc>>,
    pub last_active:  DateTime<Utc>,
}
```

---

## New MCP Tools

```
kb_agent_register(name, kind, model?, version?, parent?, project?, config?) -> AgentId
kb_agent_retire(id_or_name, purge_memories?=false) -> ()
kb_agent_list(project?, include_archived?=false) -> [AgentSummary]
kb_agent_get(id_or_name) -> AgentFull
```

---

## Insertion Semantics

- `kb_remember(source_agent=Some(name), ...)`: validate name → lookup agent by name → reject if archived → set `memory.author`.
- `source_agent=None` → `author=NULL`. All existing callers unchanged.
- Agent-authored `working` and `scratch` memories default to `visibility=Private` — prevents agent noise bleeding into cross-project recall.
- Project inheritance: if agent has a project and the memory omits project, memory inherits agent's project.

---

## Retrieval Semantics

`kb_recall(author_filter?, ...)`:

| `author_filter` value | Predicate |
|---|---|
| `"me"` | `WHERE author IS NULL` |
| `"<name>"` | `WHERE author = <agent_id>` |
| omitted | no author predicate |

- Cheap indexed field check — not graph traversal.
- Spreading activation is author-agnostic (subject graph is orthogonal to authorship).
- Queries on retired agents work unchanged — memories retain their `author` FK forever.

---

## Dedup Scoping

Dedup runs within same-author only. Cross-author near-duplicates (cosine 0.82–0.92) receive a `relates` edge at auto-link time rather than a rejection.

---

## Retire Semantics

`kb_agent_retire(id_or_name, purge_memories=false)`:
- Sets `status=archived`, `retired_at=now()`.
- Memories retain `author` reference — still queryable by historical agent name.
- `purge_memories=true` → soft-archives all memories authored by this agent (sets their `status=archived`). Never hard-deletes.
- Re-registering with the same name resurrects the agent: clears `archived`, updates `last_active`.

---

## Future Multi-User Migration Path (not v0.4)

Add `owner_user_id: UserId` column to `agent`, `memory`, and `entity`. Every query scopes by owner. No schema upheaval — single column addition per table.

---

## Consequences

### Positive
- "What did agent-X decide?" is a single indexed query.
- Retired agents stop polluting active recall automatically.
- Agent noise isolated to `Private` by default.
- Version chains queryable via `parent_agent` FK.

### Negative
- One new migration, one new table, one new column on `memory`.
- Four new MCP tools to document and maintain.
- `kb_remember` write path gains a lookup step when `source_agent` is set (name → ID resolution).

### Trade-offs accepted
- `UNIQUE` index on `agent.name` means name is the stable identity handle. Name changes require retire + re-register, not an in-place rename.
- No auto-shadow entity row for agent-as-subject in the knowledge graph — deferred until a concrete use case emerges.

---

## Out of Scope

- Edge-level author tracking (authored_by edge rejected — field FK is cheaper and sufficient).
- Per-agent ACL / permission model (single-user invariant preserved).
- Auto-shadow entity row for agent-as-subject in the knowledge graph.

---

## Implementation

- **Migration:** 006 (`src/db/migrations.rs`)
- **New files:** `src/types/agent.rs`, `src/db/agent.rs`, `src/tools/agent.rs`
- **Modified:** `src/db/memory.rs` (author field + resolution), `src/db/search.rs` (author_filter predicate), `src/types/mod.rs`, `src/tools/recall.rs`, `src/tools/recall_at.rs`, `src/tools/remember.rs`, `src/server.rs`
- **Tests:** 23 agent tests, 296 total, 0 clippy warnings
- **Shipped:** 2026-04-21

---

## Related

- ADR-009: Security & concurrency hardening (ships before this)
- ADR-011: Bitemporal model — `agent.retired_at` is the analog of memory's `invalidated_at`; the two fields are orthogonal
