# Smriti v2: step-back architecture (decision)

Status: Proposed (critic edits applied), 2026-10-24. Builds on `docs/proposals/runtime-llm-quality.md` (P0–P3, decided). Keeps ADR-011 (bitemporal), the additive-only judge invariant, Jev routing, and the rule that Sensitive is filtered on read only. Supersedes ADR-006 (SM-2). Amends ADR-020 (chunking) and ADR-024 (tool surface).

---

## 0. The fundamental memory protocol: MP/1

**Units.**
- **Record.** Immutable source bytes: `id = content hash(normalized bytes)`, globally unique, `media`, `recorded_at`, `author`, `source_ref`, `visibility`, `scope`. This is the only truth.
- **Note.** One atomic claim of bounded length, derived from at least one Record. Fields: `text`, `kind?`, `tags[]`, `event_at?`, `[valid_from, valid_until)`, `recorded_at`, `importance` (the caller's prior, immutable), `pinned`, `derived_from[]`, `scope`.
- **Entity.** A stable identity with aliases. **Link.** A directed, typed edge `(from, rel, to)` with `confidence`, `origin ∈ {caller, extractor, consolidator, auto}` and a validity window.
- **Scope.** `{id, parent?}`: a namespace (Smriti `project`, a Supermemory container). Every unit lives in one.
- **Session.** `{id, actor, scope, started_at}`. Every RECALL returns a `trace_id` within a session; implicit feedback and `kb_profile` loading key on it.
- **Event.** An append-only entry `{id, at, actor, session?, op, target, args}` for every operation, recall and feedback included. Notes, links and their states are projections of Records plus Events.

**Operations.** Five core ops; every write is idempotent.

| Op | Exact semantics |
|---|---|
| `OBSERVE(payload, source, event_at?, hints?) → record_id, note_ids` | Appends a Record and an Event carrying all hints (`summary, kind, tags, entities, importance, valid_until`). Hints become a Note now; otherwise one is derived asynchronously. A repeated hash returns the existing ids. A same-author near-duplicate **reinforces** the existing note (its `derived_from` grows); hints are never discarded. Never rejected for similarity or missing classification. |
| `RECALL(query \| id, as_of=now, filters?, k, depth=current\|all) → [note, evidence span, validity, provenance], trace_id` | Hard filters are visibility, scope and time only; `kind`/`tags` are soft boosts. Logs a recall Event with ids, ranks and scores, never text. Mutates nothing. |
| `REVISE(note, reason, text?, valid_until?) → successor?` | `update`: the world changed; old `valid_until = t`, successor `valid_from = t`, `supersedes` link. `correct`: never true; `invalidated_at = now`, `retracts` link, successor optional. `expire`: sets `valid_until`. Text is never edited. |
| `SUPPRESS(id, on\|off, reason)` | Hides a note, link or entity from default RECALL without a truth claim. Reversible; latest event wins. `reason=folded` is used by consolidation. |
| `FEEDBACK(trace_id, {note_id → signal}[])`, `FEEDBACK(note_id, pin\|unpin)` | `signal ∈ {used, irrelevant, wrong, missing}`. The only input that adapts ranking (§5). Pin/unpin need no trace. `wrong` alerts; it never retracts. |

CONSOLIDATE, LINK and INSPECT are derivable from OBSERVE/SUPPRESS/RECALL and exist as named operations for ergonomics; conformance requires only the five core ops. `CONSOLIDATE(ids, text)` ≡ `OBSERVE(text, derived_from=ids)` + `SUPPRESS(ids, folded)`; `LINK` ≡ OBSERVE of a typed claim with a validity window (unlink = `REVISE(link, expire)`); `INSPECT(id)` ≡ `RECALL(id, depth=all)`. Admin only: `REBUILD(projection)` and `PURGE(record)` (legal erasure, tombstone event).

**Invariants.**
- **I1 Append-only.** Records and Events are never mutated, except a logged PURGE.
- **I2 Provenance closure.** Every Note reaches a Record; every vector, link, tag and salience value rebuilds from Records, Events and the model fingerprint.
- **I3 Bitemporal.** `recorded_at` never changes; windows close and never reopen; `RECALL(as_of=t)` is reproducible.
- **I4 Reads never mutate units.** A recall only appends an Event.
- **I5 Salience is a prior, never a gate.** Nothing becomes unreachable through disuse.
- **I6 Bounded hot set.** Hot ⊆ active. CONSOLIDATE, REVISE and SUPPRESS only shrink the hot **count**.
- **I7 Automation is additive.** Automation may OBSERVE, LINK, tag and fold (CONSOLIDATE). It never issues REVISE or SUPPRESS; it proposes them as alerts.
- **I8 Visibility and scope are enforced on read, by actor.**

**Lifecycle.** `pending → active → superseded | retracted | expired | suppressed | folded`, all reachable with `depth=all`. Pinned notes are exempt from default expiry and demotion and form the core block (`kb_profile`). `salience` is a time-decayed fold over recall and feedback events, used only as a fusion prior and consolidation order; its formula is not protocol.

**Conformance properties.** (1) `replay(L0) == L1`. (2) `RECALL(as_of=t)` is idempotent for any fixed t. (3) `SUPPRESS on` then `off` restores the prior RECALL result. (4) No op mutates `recorded_at` or a Record. (5) OBSERVE of a known hash returns the same ids. (6) Every returned note has a provenance path to a Record. (7) No automation actor appears on a REVISE/SUPPRESS event.

**Concurrency, errors, replication.** Records are idempotent on hash alone: two actors observing the same hash with different hints get one Record, and *both* hint sets land as separate events; the note reflects the earliest, later ones reinforce. OBSERVE rejects only on visibility/scope violation (`forbidden`) or oversize payload (`too_large`). Multi-device sync is log-shipping of L0: Record ids are global content hashes, event ids are ULIDs, and each device rebuilds L1/L2 locally.

**Smriti mapping (~25 tools become 5 core + 3 sugar plus admin).**

| Today | MP/1 |
|---|---|
| `kb_remember`, `kb_queue`, `kb_entity_upsert` | OBSERVE (entity and alias hints) |
| `kb_recall`, `kb_recall_at`, `kb_entity_find` | RECALL (`as_of`, entity filter) |
| `kb_drill`, `kb_entity_get` | INSPECT (sugar) |
| `updates_id`, `kb_archive(retract)` | REVISE |
| `kb_archive(forget)`, `kb_entity_delete`, `kb_relation_retract` | SUPPRESS |
| `kb_link` | LINK (sugar) |
| `kb_consolidate` | CONSOLIDATE (sugar) |
| `kb_promote` | FEEDBACK(pin) |
| `kb_profile`, status, alerts, export (dump the log), import (replay OBSERVE), reembed and regenerate_md (REBUILD), consolidation_candidates, `kb_agent_*` | admin and read views |

---

## 1. Thesis

Smriti stores one heavy row type for three things (source, claim, index entry) and makes the caller classify each write into one of 23 unchecked bins. The fix: separate **truth** (records plus an event log) from **derived** state (notes, links, salience) and a disposable **index**. Classification becomes a hint; the system learns from recall and feedback events, not decay schedules. Nothing is lost (everything rebuilds from the log), nothing bloats (only active notes are hot), and every recall is an observation. Most of P0–P3 carries over onto the new substrate.

## 2. Diagnosis: where we lag

1. **Retrieval-bound, as already measured.** LoCoMo accuracy 63.4%, miss rate 44%, Recall@10 50%, conditional accuracy 76.5%. Notes are good keys but poor evidence: verbatim content returns only on `kb_drill`.
2. **The vector stage will not scale.** A full `SELECT *` cosine scan with no ANN (`search.rs:1074-1092`) deserializes content on every query. Vectors are `array<float>` (`migrations.rs:216`), possibly stored as f64. This, not the taxonomy, is the first ceiling.
3. **Chunks are full memories.** Content of 2000 chars or more (`chunking.rs:27`) becomes N full-schema rows with their own indexes, SM-2 fields and TTL, which then compete with their parent in fusion.
4. **Truth gets overwritten.** Prune lowers `importance` in place (`prune.rs:80-147`); its archive step is already soft (`prune.rs:192-195`), so the loss is the importance mutation, not deletion. Drill writes `updated_at` (`memory.rs:517,528,556`), which corrupts transaction time.
5. **Adaptivity has no input.** Recall records nothing (`search.rs:1589`), so SM-2 decays on a signal that is never emitted. `domain_decay_rate` has no callers.
6. **The taxonomy gates without evidence.** The 23 subtypes set TTL, floors, decay immunity and hard filters. A wrong subtype excludes gold, and nothing measures whether a subtype predicts value.
7. **Context is faked.** `voyage-context-3` (`config.rs:433`) gets a single chunk per write; a string prefix imitates native context.
8. **Structure is duplicated.** Many edge tables plus `typed_relation`, `person` beside `entity`, two recall pipelines (D6).
9. **Nothing is measured.** There is no `bench/` (none in git history), gates ran on n=196 (±7 pts), and the 281 MB has no breakdown.

## 3. Architecture

```
                 agent ──MCP──► MP/1 (5 ops)            admin: REBUILD, PURGE, status
                                   │
 L0  TRUTH  (append-only; the only thing that must be backed up)
     record  {hash id, media, payload zstd, recorded_at, author, visibility}
     event   {id, at, actor, session, op, target, args} ← every op, recall, feedback, judge verdict
 ─────────────────────────────────────────────────────────────────────────────
 L1  DERIVED  (materialized projections; rebuildable from L0 + fingerprint)
     note ──link──► note | entity(+alias)        span {record, start, end, text}
     salience = fold(events)                     kind/tags = hint | Jev
 ─────────────────────────────────────────────────────────────────────────────
 L2  INDEX  (disposable; REBUILD)
     in-process int8 matrix over ACTIVE notes + their spans  │  BM25(note.text, span.text)
     recall: BM25 ∥ int8 scan → RRF pool 150 → spans collapse to notes
             → rerank top ≤50 (Jev P1.2, Voyage fallback) → salience prior → spreading
```

Only L0 is truth. `note` is today's `memory` table, slimmed into a materialized view; caller summaries and consolidator text live in event args, so a note is never the only copy. Hot is a predicate, not a tier: the matrix holds `active` notes; the rest is one `depth=all` scan away.

## 4. Data model and taxonomy verdict

**Verdict.** The 5 types and 23 subtypes go from a required spine to optional hints. One closed enum survives: `kind ∈ {event, fact, procedure, intent}`. It is a soft boost, and only `intent` carries extra semantics (due and done). Lifecycle is handled by `valid_until` and `pinned`. Each subtype becomes a tag through a deterministic mapping, so nothing is lost. Every importance floor and ceiling is deleted.

| Today | kind | tag | Lifecycle default |
|---|---|---|---|
| episodic: meeting, conversation, daily_log, reflection, session, research | event | the subtype | none (the daily_log ceiling is dropped) |
| semantic: decision, fact, preference, metric, pattern | fact | the subtype | none (the decision≥4 floor is dropped) |
| semantic: value | fact | value | `pinned=true` |
| procedural: workflow, skill | procedure | the subtype | none |
| prospective: action_item, goal, experiment | intent | the subtype | none |
| prospective: life_goal | intent | life_goal | `pinned=true` |
| working: scratch, signal | event | the subtype | `valid_until = +48h` / `+14d` |
| working: priority, open_question | intent | the subtype | `+7d` / `+30d` |
| working: hypothesis | fact | hypothesis | `+7d` |

Legacy `type`/`subtype` map through this table (non-breaking). Unhinted writes get `kind` and tags from Jev Choice (P2.1). Per-tag fitness (used ÷ shown) will show whether any category predicts value.

**Final schema (Smriti profile).** Profile values: content hash = blake3, note text ≤500 chars, reinforce cosine >0.92, int8 × 1024 vectors, zstd payloads. 7 memory tables plus `scope`, `session`, `id_alias`, `agent` and `_kb_metadata`.

```
record  { id: blake3, media, payload: bytes(zstd), recorded_at, author, session,
          scope, source_ref, channel, visibility }                idx: author, scope
event   { id: ulid, at, actor, session?, trace_id?, op, target, args }
                                                    idx: (target, at), (op, at), trace_id
scope   { id, parent? }        session { id, actor, scope, started_at }
id_alias { old_id, new_id }    (M3: old chunk ids keep resolving)
note    { id, text≤500, kind?, tags[], scope, people[], domain[], event_at?,
          valid_from, valid_until?, recorded_at, invalidated_at?, invalidation_reason?,
          state, importance(1-5, immutable), pinned, derived_from[], anchors≤3,
          search_keys? (P2.2), salience: float, vec: bytes(1024 int8) }
                                   idx: FTS(text), FTS(search_keys), (state, project),
                                        bitemporal composite, author
span    { record, ord, start, end, text, vec: bytes(1024) }      idx: FTS(text), record
link    { in, out, rel, confidence, origin, valid_from, valid_until?, obs_event }
entity  { name, kind, roles, attrs, archived_at? }   entity_alias (unchanged, ADR-030)
```

`link.rel` subsumes every edge table and every `typed_relation` kind. `relates` auto-links carry `origin=auto` and are a rebuildable cache. `person` folds into `entity`. `consolidation_attempt` and the planned `judge_verdict` ledger become `event` rows.

## 5. Lifecycle

**Write (OBSERVE).**
1. Validate and hash. A known hash returns the existing ids.
2. Append record and event. Materialize the note from hints, or mark it `pending`.
3. One contextualized embed call per record: chunks `[header + note text, passage₁…passageₙ]`, header `[kind][project][domains][event_at ISO]`, passages sentence-bounded at ~300 tokens, no overlap; records over 32K tokens (unverified, S0) are windowed. The local embedder uses the P0.8 prefix.
4. Same-author note cosine >0.92 (profile value) reinforces: it appends an event carrying the full hint set; a differing `importance` or `event_at` is preserved in the event, and `derived_from` grows. 0.82–0.92 goes to the P2.3 gray band.
5. Async: entity linking, relations with candidate IDs (P1.3), anchors, tags.

**Consolidate (replay).** The ADR-026 auto-consolidator runs hourly and is the only automated writer of derived notes. Each tick it folds clusters through CONSOLIDATE (behind the P2.4 gates), recomputes salience, mines co-retrieval pairs (P3), and proposes supersessions as alerts. Clusters are processed in salience × recency order, which is prioritized replay.

**Adapt.** RECALL emits a `trace_id` (stored on the event, within a session), and the caller sends FEEDBACK. Per signal:
- `used` → +salience for the note, and a co-retrieval edge to other `used` notes in the trace.
- `irrelevant` → −salience for *this query cluster* only (keyed on the trace's query vector), never globally.
- `missing` → nothing adaptive (there is no id); logged as a bench miss.
- `wrong` → alert to the owner plus a `disputed` tag; no retraction (I7).
- `pin`/`unpin` → toggles `pinned`.

Two implicit signals count as weak `used`: an INSPECT in the same session, and a later OBSERVE that cites the id; both need the session/trace fields above. Salience feeds fusion, with its weight fitted on the dev split. Recall events are the offline learning-to-rank set for the RRF k, the rerank weight `w` and the tag boosts.

**Decay without forgetting.** Nothing decays in storage. Read-side only: low salience ranks lower, a passed `valid_until` hides from `depth=current`, a folded note leaves the hot index. All are visible to INSPECT and reversible.

## 6. Voyage usage plan

| Use | Decision |
|---|---|
| Document embedding | `voyage-context-3` at 1024 dimensions (current), called *contextually* (§5). Run `voyage-context-4` (unverified, S0: existence at 1024d) as an M1 bench arm and adopt it on Recall@150 parity: (unverified, S0) $0.12/M against $0.18/M and a 200M free tier. No unverified figure is a gate input. |
| Query embedding | The same model with `input_type=query`. No mixed-size or mixed-family pairing; the shared-space claim is unverified. |
| Dimensions | 1024. No Matryoshka truncation without a bench result. |
| dtype | Request float (`output_dtype` on context models: unverified, S0) and quantize to int8 locally, with per-dimension scales in `_kb_metadata`. Drop float after Recall@150 parity. Binary prefilter only past 10⁶ active notes. |
| Fingerprint | `provider/model/dim/dtype/mode`, gated at startup (Migration 015, extended). |
| Batch API | Only for `REBUILD`/`kb_reembed` (33% off and contextualized-endpoint coverage: unverified, S0). **One** re-embed window, in M1, covers contextual mode, int8 and context-4, replacing the P0.8 re-embed. |
| Rerank | `rerank-2.5-lite` as the Voyage path, Jev primary per P1.2. Budget: ≤50 docs × ≤150 tokens (ISO date, validity, note, evidence ≤400 chars) ≈ 8.5K tokens/query. `rerank-2.5` is a bench arm. |
| Multimodal | Not now; `record.media` keeps the door open. |

**Cost per 10k memories** (~330 tokens each: note 60, header 15, content ~250).

| Item | Tokens | List | With free tier / Batch |
|---|---|---|---|
| Initial embedding | 3.3M | (unverified, S0) $0.40 (context-4) / $0.60 (context-3) | (unverified, S0) $0 / $0.40 Batch on context-3 |
| Full re-embed | 3.3M | same | (unverified, S0) ×0.67 via Batch |
| 1,000 recalls (rerank-lite) | 8.5M | $0.17 | free for the first ~23k recalls |
| Query embedding | ~15 tok/query | negligible | — |

## 7. Storage and growth model

The per-memory targets below are estimates. M0 replaces them with measured bytes.

| Component | Today (est.) | Target |
|---|---|---|
| Source content | content field plus N chunk rows | zstd record ≈ 0.4× raw, plus span text for BM25 ≈ 1× raw |
| Row overhead | full schema per chunk | note ≈ 0.4 KB; span ≈ 0.1 KB plus text |
| Vectors | 4–9 KB per row, including chunks | 1 KB int8 × (1 + spans) |
| Edges and ledgers | 8 edge tables plus ledgers | link plus event ≈ 0.5 KB per memory |
| **Per memory, 1.5 KB of raw content** | **~10–20 KB** | **~3.5 KB** (10k → 35 MB; 10⁶ → 3.5 GB) |

**Growth law.** Total bytes grow as O(compressed input), which is the minimum possible under "nothing lost". The hot cost tracks the active set, not history:
- The int8 matrix at 10⁵ active units is about 130 MB of RAM, with a SIMD scan of about 10–20 ms (estimated).
- Supersede, fold and suppress shrink the matrix (hot count, not total bytes).
- The event log grows with recalls, not writes: ~150 B × every recall (10⁴/day ≈ 0.5 GB/yr).
- **Event log compaction:** recall events older than N days fold into a salience snapshot event per note; raw events beyond that are purged *as derived data* (they are reproducible from the snapshot only approximately — this is the one lossy compaction, and it is owner-approved).
- Hot-budget demotion is deferred until CONSOLIDATE proves insufficient.

## 8. What we delete

Required `type`/`subtype`, floors, ceilings and hard type filters; SM-2 fields, `sm2_decay`, `archive_below_threshold`, in-place importance mutation and `domain_decay_rate` (supersedes ADR-006); drill's `access_count`/`updated_at` writes; chunk-as-memory rows, content FTS over 500 KB blobs and P2.7 per-chunk summaries; dedup rejection (becomes reinforcement) and `working` as a type (becomes `valid_until`); `person`, the separate edge tables, `consolidation_attempt` and `judge_verdict`; `recall_at` and `kb_promote`; the `SELECT *` vector scan, float vectors after int8 parity, and B-tree indexes on type, importance and domain.

## 9. Composition with P0–P3

| Item | Verdict |
|---|---|
| P0.1 bench | **Keep. Prerequisite for everything.** Add LongMemEval, byte accounting and the full LoCoMo10. |
| P0.2 MMR off, P0.3 Sensitive enrichment, P0.4 sidecar model config | Keep (in progress on this branch). |
| P0.5 pool 150, P0.6 `primary_cap` removal, P0.7 rerank policy | Keep. Flip `pool_size` to 150 after measuring. |
| P0.8 content prefix | **Replace** on Voyage with the *contextual note vector*: send `[header+summary, passages]` through the existing `chunking.rs` contextual path and keep chunk 0's vector. No schema change. The prefix stays for the local embedder. |
| P1.1 Judge trait, P1.2 Jev rerank, P1.3 relation candidates, P1.5, P2.1–P2.6 | Keep. The ledger becomes `event(op=verdict)`; the rerank document gains the evidence span and ISO date; P1.3 is the D5 fix spreading depends on; P2.2 `search_keys` is a derived note field; a P2.3 restatement *reinforces* instead of *rejects*. |
| P1.4 `recall_at` parity | **Replace** with deletion: `recall(as_of)`. |
| P2.7 anchor Choice | Keep anchors. Drop the per-chunk summaries. |
| P3 reconsolidation, distillation, LocalJudge | Keep. Inputs come from recall events; distillation is CONSOLIDATE into `kind=procedure`. |
| P3 importance judge (W2) | **Replace.** Importance is immutable, and the judge may only propose `pin`. |
| P3 `auto_supersede` | Keep its gate. Under I7 it only proposes a REVISE as an alert; the owner applies it. |

## 10. Phased migration with bench gates

Gates: full LoCoMo10, paired tests, held-out split, synthetic guard. Old tool names alias until M5.

| Phase | Work | Gate |
|---|---|---|
| **M0** (this branch) | Finish P0. Create `bench/`. Add byte accounting to `kb_status`. Stop drill writing `updated_at`. (Private cross-project policy moves to §11.8.) | A new baseline is recorded with ≤±1 pt spread over 3 runs, and the 281 MB is broken down. |
| **M1** Vector path | Select only `id, vec`. Build the in-process int8 matrix. Add the contextual note vector and the context-4 arm. Run the single Batch re-embed. | int8 matches float on Recall@150, Recall@10 is at least the prefix arm, and p50 drops by 30%. |
| **M2** Truth layer (Migration 022) | Add `record` and `event`, and have the write path write them first. Backfill from summary and content. Move the ledgers into `event`. Add `scope`, `session`, `trace_id`, `actor` and FEEDBACK. Import replay accepts an admin-only `recorded_at` override so restore keeps I3. | 100% provenance closure, write p95 rises by 10 ms or less, and an event replay reproduces `note` on a sample. |
| **M3** Spans (Migration 023) | Convert chunk rows to `span` (old rows go to `folded`; `id_alias` maps old chunk ids). Use one contextual call per record. Recall returns evidence spans. | No Recall@10 regression, conditional accuracy rises, and rows and bytes fall. |
| **M4** Taxonomy | Make `type`/`subtype` optional and backfill kind and tags. Turn floors off. Map working types to `valid_until` and `is_static` to `pinned`. Remove SM-2. | No Recall@10 loss, and the synthetic decay probes are at least the baseline. |
| **M5** Unify | Merge into one `link` table. Fold `person` into `entity`. Delete `recall_at` and `kb_promote`. Cut tools to 5 core + 3 sugar plus admin. | Contract tests pass, and LongMemEval temporal and knowledge-update scores are at least M4. |
| **M6** Adapt | Add the salience prior (ACT-R and Beta arms), the P3 items, and learning-to-rank from events. | Recall@1 rises 2 pts on held-out data with no Recall@10 loss. Otherwise salience stays unweighted. |

Every phase is reversible: L0 is never rewritten, so REBUILD rolls back any L1/L2 change.

## 11. Open decisions for the owner

1. **Approve the ADR changes.** Supersede ADR-006 and amend ADR-020 and ADR-024. Also change the CLAUDE.md rule "Type + subtype REQUIRED" to "optional hints". *Recommend: yes.*
2. **PURGE.** Allow admin-only legal erasure with a tombstone? It is the single exception to "nothing lost". *Recommend: yes, never exposed on the agent surface.*
3. **Tool contract.** Keep the ~25 tool names as aliases for one minor version, then cut to 5 core + 3 sugar plus admin? *Recommend: yes.*
4. **Query vectors in the event log.** Store an int8 query vector, but never query text? This is needed for learning-to-rank. *Recommend: yes, local only.*
5. **One re-embed window (M1)** in place of the P0.8 re-embed, covering contextual mode, int8 and possibly context-4. *Recommend: approve.*
6. **Ingest anything** (PDF, OCR, audio), Supermemory-style: now or after M3? *Recommend: after M3, as sidecar adapters that produce text records.*
7. **Implicit `used` signals** (INSPECT in the same session, a citation in a later OBSERVE): accept them as feedback, or use explicit FEEDBACK only? *Recommend: accept them, weighted below explicit feedback.*
8. **Private cross-project search policy.** Private is project-scoped; cross-project queries return Standard only (`search.rs:428,786,1078`, consistent across all stages). This is a documented privacy contract, not a bug; change it only by owner decision. *Recommend: keep.*
9. **Event-log compaction loss.** Approve folding old recall events into salience snapshots, the one approximate compaction of derived data? *Recommend: yes, with N set after M2 measures growth.*
