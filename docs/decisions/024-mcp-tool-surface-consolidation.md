# ADR-024: MCP Tool Surface Consolidation

**Status:** Proposed — 2026-04-30
**Date:** 2026-04-30
**Extends:** ADR-011 (bitemporal memory), ADR-010 (agent authorship)

## Context

smriti exposed 28 MCP tools. Anthropic published degradation past 30–50 tools in the
context window; MCPVerse benchmarks corroborate. Researcher review found three clear
redundancies inside the current surface; devils-advocate confirmed 28 is still inside
the safe zone but flagged the redundancies as avoidable decision burden on callers.

Three pairs of tools overlap in semantics:

1. `kb_people` duplicates `kb_entity_find(kind="person")`. It is already legacy-marked
   at `src/server.rs:521`.
2. `kb_forget` and `kb_retract` are both soft-archive operations. The distinction
   between them (intent vs. correction) forces caller disambiguation that the tool
   boundary itself should carry.
3. `kb_agent_list` and `kb_agent_get` are both reads on the agent table, distinguished
   only by whether a lookup key is present. Merging them avoids two tool slots for
   one table.

## Decision

Three consolidations, net 28 → 25 tools. No legacy aliases. All removals are
backward-incompatible; callers referencing removed names break.

### 1. Drop `kb_people`

Remove the handler and its tool registration from `src/server.rs`. Callers use
`kb_entity_find(kind="person")` directly. No behavior change — the underlying query
path is identical.

### 2. Merge `kb_forget` + `kb_retract` → `kb_archive`

```
kb_archive(id, reason: ArchiveReason, correction_id?) -> {message}
```

Add `ArchiveReason` to `src/types/enums.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveReason {
    Forget,
    Retract,
}
```

The `Retract` arm accepts an optional `correction_id`. When provided, a `retracts`
edge is created from `id` to `correction_id`. Sets `invalidation_reason=retracted`.
The `Forget` arm sets `invalidation_reason=forgotten`. Both arms set `status='archived'`
and `invalidated_at`. Invariant unchanged: `status='archived'` iff `invalidated_at IS NOT NULL`.

### 3. Merge `kb_agent_list` + `kb_agent_get` → `kb_agent_query`

```
kb_agent_query(id_or_name?, project?, include_archived?=false) -> AgentQueryOutput
```

```rust
pub struct AgentQueryOutput {
    pub single: Option<AgentFull>,
    pub list: Vec<AgentSummary>,
}
```

When `id_or_name` is provided, `single` is populated and `list` is empty. When
`id_or_name` is absent, `list` is populated and `single` is `None`. Flat struct
avoids `anyOf` in the JSON schema — GPT-4o and Gemini drop ~30% on tagged unions
(MCPVerse finding).

### Scope unchanged

`kb_reembed` and `kb_regenerate_md` remain on the MCP surface. User accepted the
Voyage-budget attack-surface implication of not moving them to a CLI subcommand.

## Consequences

### Positive

- 25 tools total. Inside Anthropic safe zone with headroom.
- `ArchiveReason` centralises intent that was previously split across two tool names.
- `AgentQueryOutput` flat struct is safe across all major MCP clients.
- `kb_people` removal clears dead code flagged in `src/server.rs`.

### Negative — must accept

- Backward-incompatible. `kb_people`, `kb_forget`, `kb_retract`, `kb_agent_list`,
  `kb_agent_get` references in downstream callers and test files break at implementation.
- Test files using removed or renamed tools must be updated as part of implementation.
- CLAUDE.md tool signatures section must be updated.

### Trade-offs accepted

- No legacy aliases — clean removal over shim layer. User accepted breakage.
- Flat output struct for `kb_agent_query` over a discriminated union. Loses some
  type precision; gains client compatibility.

## Alternatives considered

- **Keep all 28 with `defer_loading: true` on cold tools** — rejected. User wants
  real removal, not hiding.
- **Aggressive 28 → 12** — rejected. Action-param antipattern; output-type collisions;
  merging `kb_recall` and `kb_recall_at` violates ADR-011's bitemporal contract.
- **Move `kb_reembed`, `kb_regenerate_md` to CLI subcommands** — rejected by user.
  User accepts the Voyage-budget attack-surface risk.

## Related

- ADR-010: Agent authorship (`kb_agent_query` replaces two tools on the agent table).
- ADR-011: Bitemporal memory (archive invariant unchanged by tool merge).
- ADR-025: Memory librarian subagent (further reduces tool surface visible to main
  Claude session by routing writes through a subagent).
