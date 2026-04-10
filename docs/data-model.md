# Data Model

## Memory Record

Every memory has:
- **summary** — 10-500 chars. Injected on retrieval. REQUIRED.
- **content** — full content. Fetched only on `kb_drill()`. Optional.
- **source_ref** — path/ID of original raw file. Never injected.
- **memory_type** + **subtype** — from the 23-subtype taxonomy
- **source** — input channel: manual, meeting, slack, discord, code, chat
- **project** — primary project name (optional)
- **domain** — life areas: health, learning, philosophy, etc. (free-form, multi-valued)
- **people** — resolved to entity records via `involves` edges
- **importance** — 1-5, with per-subtype floors/ceilings
- **visibility** — standard, private, sensitive
- **status** — active, archived, superseded
- **event_at** — WHEN it happened (bi-temporal: distinct from created_at)
- **created_at** / **updated_at** — storage timestamps
- **expires_at** — TTL for working/* subtypes
- **access_count** — incremented on every recall hit
- **tags** — free-form categorization
- **source_agent** / **session_id** — provenance tracking

## Importance Floors/Ceilings

| Subtype | Floor | Ceiling | Rationale |
|---------|-------|---------|-----------|
| decision | 4 | 5 | Decisions are permanent |
| value | 5 | 5 | Core beliefs |
| life_goal | 5 | 5 | Life goals |
| action_item | 3 | 5 | Tasks must surface |
| goal | 4 | 5 | Goals matter |
| scratch | 1 | 2 | Ephemeral |
| daily_log | 1 | 2 | Context, not critical |
| (others) | 1 | 5 | No enforcement |

## Entity System

Flexible, schemaless entities:
```
entity {
  name: String,           // unique key
  kind: String,           // person, company, product, concept, project
  roles: Vec<String>,     // investor, advisor, customer, etc.
  attrs: Object,          // freeform key-value (includes last_contact for persons)
  context: Option<String>,// one-line summary
}
```

- `attrs.last_contact` auto-updated when entity is linked to a memory via `link_entity`
- Stale contact alerts fire when `last_contact` exceeds configured threshold
- Projects can be entities with lifecycle attrs (status, killed_reason, pivoted_to)

## Graph Edges

```
memory  ->involves->  entity       (mentioned, present at meeting)
memory  ->assigned->  entity       (action item assignee — auto-wired for action_item subtype)
memory  ->updates->   memory       (supersession — old marked as superseded)
memory  ->extends->   memory       (adds to without contradicting)
memory  ->derives->   memory       (consolidation provenance)
memory  ->relates->   memory       (semantic similarity)
```

### Auto-Link Thresholds

| Condition | Threshold | Edge |
|-----------|-----------|------|
| Same project, cosine > 0.90 | 0.90 | `extends` |
| Same project/domain, cosine > 0.75 | 0.75 | `relates` |
| Cross-domain, cosine > 0.85 | 0.85 | `relates` |

- 8 similarity edges max per memory (involves edges uncapped)
- Entity name matching: whole-word boundary, min 4 chars

## Visibility

Enforced in ALL query paths:
- `standard` — normal retrieval, appears in markdown layer
- `private` — only on explicit domain/direct query, never cross-project
- `sensitive` — never in markdown, never logged, query-only with explicit ID/domain

## Decay & Pruning

**TTL expiry** — working/* subtypes have mandatory TTLs (scratch=48h, priority=7d, etc.)

**SM-2 decay** — memories past review date with no access lose importance. Excludes:
- Importance ≥ 5 (immune)
- Subtypes: decision, value, life_goal (immune regardless of importance)
- Working/* (TTL handles them)

**Domain-aware rates** — health/fitness at 0.5x, philosophy/personal at 0.25x

**Archive threshold** — importance falls to configured min → archived

**Supersession** — `updates_id` marks old memory as superseded (verified before executing)

Rule: **never hard-delete**. `WHERE status = 'active'` filters archived/superseded.

## Bi-Temporal Timestamps

- `event_at` — when the thing happened (the meeting was Tuesday)
- `created_at` — when stored in DB (today)
- Temporal queries use `event_at`, not `created_at`
