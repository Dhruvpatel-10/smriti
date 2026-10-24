# Final critique

**Final critique: Smriti v2 / MP/1 draft**

Code spot-checks (worktree, read-only) confirm: brute-force `SELECT *` cosine scan (`src/db/search.rs:1089`), `option<array<float>>` (`migrations.rs:216`), drill writes `access_count += 1, updated_at = time::now()` (`memory.rs:517,528,556`), `domain_decay_rate` has no callers, dedup 0.92/0.82 (`dedup.rs:10-11`), contextualized endpoint only in `chunking.rs:279-300`. Two draft claims do **not** hold up (below).

## Verdicts

| Criterion | Verdict | Why |
|---|---|---|
| Fundamental (minimal, complete, impl-independent) | **Fail (fixable)** | Not minimal: `CONSOLIDATE(ids, text)` ≡ `OBSERVE(text, derived_from=ids)` + `SUPPRESS(ids, reason=folded)`. `LINK` ≡ `OBSERVE` of a typed claim with a validity window (the draft already says unlink is `REVISE(link, expire)`). `INSPECT` ≡ `RECALL(id, depth=all)`. So the irreducible core is **five**: OBSERVE, RECALL, REVISE, SUPPRESS, FEEDBACK. Keep CONSOLIDATE/LINK/INSPECT as *named sugar* but say so, or the "TCP/IP" claim is false. Not impl-independent: `blake3`, `500 chars`, `0.92`, `int8`, `1024`, `zstd` are inside the protocol text. Not complete: no `scope`/namespace primitive (Supermemory containers, Smriti `project`) and no actor/identity primitive — yet I8 and the author-scoped dedup depend on both. |
| Simple | **Pass, with a leak** | Two definitions of MP/1 (§0 and cross-exam §4) disagree: 8 vs "6+UNLINK/UNSUPPRESS", `depth=current\|all` vs `hot\|all`, FEEDBACK signature `(trace_id\|note_id, signal)` vs `(trace_id, used[], wrong[], pin?)`, I7 (owner-delegated REVISE allowed vs forbidden). Pick one. |
| Powerful | **Pass** | Bitemporal `as_of`, evidence spans, provenance closure, replayable adaptivity — this is more than either reference system. |
| No bloat | **Pass, argument incomplete** | O(compressed input) for L0 is correct. Missing: the **event log** is unbounded and grows with recalls, not writes (150 B × every recall; a busy agent does 10⁴/day = 1.5 MB/day = 0.5 GB/yr). "Kept raw until 1 GB" then what? Define the compaction: fold recall events into salience snapshots, keep only the last N raw. Also I6 "CONSOLIDATE shrinks" is false unless the new note is *smaller than the sum*; it strictly reduces hot *count*, not bytes. Say "hot count". |
| Nothing forgotten | **Pass, two silent-loss paths** | (a) §5 step 4 "cosine >0.92 reinforces, no new note is made": the Record is kept but the caller's *hint fields* (importance, tags, event_at) for the second write are dropped unless the event args hold them. State explicitly: hints always land in the event; reinforce never discards them. (b) `kb_import` as "replay OBSERVE" loses original `recorded_at` unless OBSERVE accepts an explicit `recorded_at` override on an admin flag; otherwise I3 is violated on restore. (c) M3 "ids aliased" for chunk→span conversion: old chunk ids referenced by external callers must resolve; the alias table is not in the schema. |
| Adaptive (concrete loop) | **Fail (fixable)** | The loop stops at "salience is a fusion prior, weight fitted on dev split". No stated behaviour for `irrelevant`, `missing`, `wrong` beyond "alert". Specify: `used` → +salience & co-retrieval edge; `irrelevant` → −salience for *this query cluster*, not globally; `missing` → nothing adaptive (no id), only a bench miss; `wrong` → alert + tag. Also: "two implicit signals" (INSPECT in session, citation in later OBSERVE) are unimplementable until trace_id/session is a schema field; it is not in `note` or `event`. |
| Efficient Voyage use | **Pass, with unverified facts** | Contextual call per record and query-side rerank cap are right. Unverified: context-4 price $0.12/M and 200M free tier; context-4 existence at 1024d; `output_dtype` on context models (draft says "unverified" then budgets on it); 32K tokens/doc limit; Batch API 33% off and whether it covers the contextualized endpoint at all. Mark every one as S0 vendor-check items; none may be a gate input. |
| Builds on prior work | **Pass, one error** | P0–P3 composition table is honest. But "Restore `bench/`" — `git log --all -- bench` is empty in this repo; nothing to restore. Rename to "Create `bench/`" and drop the "baseline reproduces" gate language that assumes an old baseline. |

## Factual corrections required

1. **D10 is not a bug.** `search.rs:428,786,1078` document the same contract three times: Private is project-scoped; cross-project queries return Standard only "to prevent cross-project private leaks". The main query, vector stage and spreading-activation neighbours all agree. Changing it is a **privacy-policy decision for the owner**, not an S0 fix. Delete "This is a bug" and move it to §11 as open decision 8.
2. **`archive_below_threshold` is already a soft archive** (`prune.rs:192-195`, `invalidation_reason='expired'`), not data loss. The real loss is `importance` mutation; keep that claim, drop the implication that prune deletes.
3. "25 tools" — count the listed surface; CLAUDE.md shows 24 plus `kb_profile`. Say "~25".

## Required edits (exact)

- §0 Operations: add a sentence "CONSOLIDATE, LINK and INSPECT are derivable from OBSERVE/SUPPRESS/RECALL and exist as named operations for ergonomics; conformance requires only the five core ops."
- §0 Units: replace `blake3` with "content hash"; move `≤500`, `0.92`, `int8`, `1024` to §4/§5 as Smriti profile values.
- §0 add Unit **Scope** `{id, parent?}` and field `actor` on every Event; I8 becomes "visibility and scope are enforced on read, by actor".
- §0 FEEDBACK: one signature. Recommend `FEEDBACK(trace_id, {note_id → signal}[])`, `pin/unpin` by note_id without trace.
- §0 I7: keep the *cross-exam* ruling (automation never REVISE/SUPPRESS; FOLD only), delete the "delegation flag" clause — it reopens the judge invariant.
- §0 I6: "shrinks the hot **count**".
- §5 step 4: "reinforce appends an event carrying the full hint set; a differing `importance` or `event_at` is preserved in the event, and `derived_from` grows."
- §7: add "Event log compaction: recall events older than N days fold into a salience snapshot event per note; raw events beyond that are purged *as derived data* (they are reproducible from the snapshot only approximately — state this as the one lossy compaction and make it owner-approved)."
- §10 M0: "Create `bench/`"; split D10 into §11.
- §10 M3: add `id_alias {old_id, new_id}` to schema.
- §6: prefix every context-4 number with "(unverified, S0)".
- §11 add: 8. Private cross-project search policy; 9. event-log compaction loss.

## Missing pieces

- **Conformance tests** for the protocol (a 20-line property list: replay(L0) == L1; RECALL(as_of) idempotent; SUPPRESS round-trip; no op mutates `recorded_at`). Without them "implementation-independent" is unfalsifiable.
- **Session/trace** as a first-class unit — needed by implicit feedback, by `kb_profile` loading, and by LongMemEval.
- **Concurrency/ordering** — two agents OBSERVE the same hash with different hints; which Note? Idempotency on hash alone is underspecified.
- **Error model** — what OBSERVE returns on a visibility violation or oversize payload (the only rejections left).
- **Multi-device sync** (owner goal "across device" in CLAUDE.md): an append-only log is the right substrate; say that replication is log-shipping and that Record ids are global.