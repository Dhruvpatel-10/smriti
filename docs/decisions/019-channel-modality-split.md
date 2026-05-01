# ADR-019: Split `Source` into `Channel` + `Modality`

**Date:** 2026-04-30
**Status:** Accepted

## Context

The current `Source` enum (`src/types/enums.rs:185`) has six variants:
`Manual | Meeting | Slack | Discord | Code | Chat`. It is a required field on
`kb_remember`.

Three orthogonal axes are collapsed into one flat enum:

- **Channel** (where bytes arrived): Slack, Discord
- **Content shape** (what form they took): Meeting transcript, Chat exchange
- **Entry method / format**: Code snippet, Manual paste

This collapse is irresolvable at ingest. A Slack export of a meeting transcript is
simultaneously `Slack` and `Meeting`. A voice memo pasted manually is simultaneously
`Manual` and `Meeting`. The caller must pick one and discard signal from the other.

In practice `Source` does no work:

- It is not embedded in `validation::contextual_text` (only type/subtype/project/
  domains/people/summary contribute).
- It is not a `RecallInput` filter.
- It does not affect dedup, visibility defaults, decay/TTL, or alerts.

A required field that does nothing — LLM callers default to `Chat`, destroying
whatever signal exists.

Coverage is also incomplete. Email, voice/audio transcripts, PDFs, web articles,
GitHub issues, and SMS have no fitting variant; all collapse to `Manual` or `Chat`.
Auto-ingest pipelines (Slack scrape) and manual user entry share the same
`Visibility::Standard` default — wrong trust calibration.

Peer survey confirms the pattern:

| System | Approach |
|---|---|
| Khoj | `file_source` × `file_type` — channel and format split |
| Readwise | typed `category` enum + free-text `source` + filterable category |
| mem0 | source dropped entirely; uses topic categories |
| Letta / LangGraph | no source schema field |

Khoj and Readwise — the two systems that retain source semantics — both split channel
from format. Keeping a flat enum is the outlier pattern.

## Decision

1. Replace the `Source` enum with two new optional enums:

   ```rust
   pub enum Channel {  // where bytes came from
       Cli, Slack, Discord, Email, Sms, Web, Api,
       Github, VoiceApp, Notion, Calendar,
   }

   pub enum Modality {  // what shape they were in
       Text, Transcript, Document, Code, Structured, Image,
   }
   ```

2. Both fields are **optional** on `kb_remember`. The required-field-that-does-nothing
   tax is dropped.

3. Both fields are embedded into `validation::contextual_text`:
   `[channel/modality] [type/subtype] [project] ...` — dense and BM25 indexes pick
   them up, enabling queries like "what did Priya say on Slack".

4. `channel?` and `modality?` filters added to `RecallInput` and `kb_recall`.

5. Source-aware `Visibility` default: memories arriving via `Slack | Discord |
   VoiceApp | Sms` default to `Visibility::Private` unless the caller specifies
   otherwise. Manual/API channels retain `Visibility::Standard`.

6. Deterministic backfill of existing rows from old `Source` (one migration):

   | Old `source` | New `channel` | New `modality` |
   |---|---|---|
   | `Manual` | `Cli` | `Text` |
   | `Meeting` | _(none)_ | `Transcript` |
   | `Slack` | `Slack` | `Text` |
   | `Discord` | `Discord` | `Text` |
   | `Code` | `Cli` | `Code` |
   | `Chat` | `Api` | `Text` |

7. The old `source` column is retained on the row temporarily for read-compat and
   dropped in a follow-up migration after all clients have migrated.

## Consequences

### Positive

- `Channel` and `Modality` become real retrieval signals: dense match for "Slack
  meeting transcript", recall filter `kb_recall(channel="slack")`.
- Auto-ingest pipelines (Slack, Discord, voice) receive safer default visibility
  without caller action.
- Coverage extends to email, document, web, GitHub, voice — five workflows currently
  impossible to label correctly.
- Callers no longer required to guess a category on every write; both fields optional.

### Negative

- Migration touches every memory row. The mapping is deterministic but pre-expansion
  `Manual` rows cannot be retroactively reclassified to `Email`, `Web`, etc. — that
  signal is gone.
- MCP tool schema breaking change: `source` field replaced by `channel` + `modality`.
  Clients must update field names. Offset by accepting `source` as deprecated input
  for one minor release if pragmatic.
- Two enums (~17 variants total) to maintain versus one enum (6 variants).

### Trade-offs accepted

- Backfill completeness loss on legacy `Manual` rows in exchange for correct semantics
  going forward.
- Short-lived schema divergence (old column retained) in exchange for zero-downtime
  migration.

## Alternatives considered

- **Expand flat enum to ~12 variants** — still has axis-collision. Khoj and Readwise
  independently converged on splitting; keeping flat is the outlier.
- **Drop `source` entirely (mem0 approach)** — loses the trust-calibration win on
  auto-ingest channels and the visibility-default signal.

## Related

- ADR-016: Voyage AI cloud embedder — embedding `channel/modality` into `contextual_text`
  gives Voyage's asymmetric retrieval more discriminating signal.
- ADR-013/014: Sidecar typed-relation and anchor extraction — `Modality` could later
  route extraction logic (e.g., skip `Code` modality for relation extraction).
