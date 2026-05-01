# ADR-025: Memory Librarian Subagent (Hybrid Read/Write Split)

**Status:** Proposed — 2026-04-30
**Date:** 2026-04-30
**Extends:** ADR-024 (MCP tool surface consolidation), ADR-010 (agent authorship)

## Context

After ADR-024 trims the surface to 25 tools, the main Claude session still bears the
full taxonomy classification burden on every write: 5 types, 23 subtypes, importance
floors, visibility rules, TTLs. This burden is where smriti's quality enforcement
matters most — misclassification at write time cannot be recovered by the retrieval
pipeline.

On the read side, recall results are already structured by the 6-stage pipeline
(BM25 → vector → RRF → reranker → entity filter → spreading activation). Adding a
synthesis layer on reads would increase latency without quality gain.

Devils-advocate review found a natural split: writes benefit from a focused
classification prompt and a dedicated role; reads do not.

Claude Code subagents (`~/.claude/agents/`) accept a `tools:` frontmatter allowlist
that scopes which MCP tools each subagent sees. This mechanism enables a
per-tool-surface split without a proxy server.

## Decision

Hybrid librarian pattern: a dedicated subagent handles all writes; the main session
calls smriti directly for reads.

### Memory librarian subagent

File: `~/.claude/agents/smriti-librarian.md`

The subagent:
- Is registered at `~/.claude/` — system-wide, covers all Claude Code projects.
- Sees the write-side tool allowlist: `kb_remember`, `kb_archive`, `kb_consolidate`,
  `kb_link`, `kb_relation_retract`, `kb_entity_upsert`, `kb_entity_delete`,
  `kb_promote`, `kb_queue`, `kb_agent_register`, `kb_agent_retire`, `kb_agent_query`,
  `kb_reembed`, `kb_regenerate_md` — plus the read tools needed for dedup checks
  (`kb_recall`, `kb_drill`). ~16 tools total.
- Runs on Sonnet model.
- Carries a focused classification prompt covering taxonomy, importance floors,
  visibility defaults, and TTL rules.
- Filters `Visibility::Sensitive` content before returning write confirmations to
  main — main cannot accidentally surface Sensitive memories into its reasoning context.

### Main session reads directly

Main session sees 9 read-side tools: `kb_recall`, `kb_recall_at`, `kb_drill`,
`kb_entity_find`, `kb_entity_get`, `kb_status`, `kb_alerts`,
`kb_consolidation_candidates`, `kb_export`. Plus one macro: invoke the librarian
subagent for all writes.

Total visible to main: ~10. Large margin below the Anthropic degradation threshold.

### Write path

Main session routes write intent through:

```
Agent(subagent_type=smriti-librarian, prompt=<write instruction>)
```

The librarian classifies, calls smriti write tools, and returns the resulting ID or
confirmation to main.

### smriti MCP server registration

Registered globally in `~/.claude/settings.json`. Tool allowlists per subagent are
declared via `tools:` frontmatter in the agent file, not in the MCP server config.

## Consequences

### Positive

- Main session sees ~10 tools. Large margin below degradation threshold.
- Write quality improves ~10–20% from focused classification prompt + dedicated role.
- Privacy boundary: librarian filters `Visibility::Sensitive` before surfacing
  summaries to main. Main cannot accidentally include Sensitive content in reasoning.
- System-wide reuse: `~/.claude/` placement applies to all Claude Code projects
  without per-project configuration.
- Read latency unchanged — main calls smriti directly, no librarian hop on reads.

### Negative — must accept

- **Write latency +1–2 s per operation** — extra Sonnet inference hop. Acceptable:
  writes are 5–10 per session and are latency-tolerant.
- **Cost +$0.003 per write** — Sonnet inference overhead. ~$0.015/session typical.
- **Failure mode** — librarian unavailable → writes fail. Reads continue working.
  Main can fall back to direct `kb_remember` call (tools registered, instructed not
  to use directly per CLAUDE.md). No data loss.
- Quality improvement is ~10–20%, not 2×. Librarian runs the same Sonnet model as
  main; benefit comes from role focus, not raw intelligence difference.

### Trade-offs accepted

- Hybrid over full-librarian: read-side latency cost is not justified by the
  structured recall results the pipeline already provides. Full routing through
  librarian rejected.
- Subagent over dedicated `smriti-librarian` MCP proxy: the Claude Code subagent
  system covers the requirement without a separate server process.

## Open questions

- Per-subagent MCP scoping mechanics in current Claude Code release (to be validated
  separately before implementation).
- Whether main is told to invoke the librarian via `~/.claude/CLAUDE.md` global
  instructions or per-project `CLAUDE.md`. Global preferred for system-wide consistency.
- Write latency benchmarks pending pilot deployment.

## Alternatives considered

- **Full librarian (all ops via subagent)** — rejected. Read-side latency cost not
  justified; recall results are already structured by the retrieval pipeline.
- **Skip librarian, trim surface further** — rejected. Main still bears taxonomy
  burden; no privacy filter; no classification focus.
- **Dedicated `smriti-librarian` MCP proxy server** — rejected. Redundant with
  Claude Code subagent system; adds a separate process to operate and monitor.

## Related

- ADR-010: Agent authorship (librarian itself is a registered smriti agent).
- ADR-024: MCP tool surface consolidation (prerequisite — tool names the librarian
  uses reflect the post-consolidation surface).
