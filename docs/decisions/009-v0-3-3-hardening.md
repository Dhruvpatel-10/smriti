# ADR-009: v0.3.3 Security & Concurrency Hardening

**Status:** Accepted  
**Date:** 2026-04-21  
**Context:** Pre-daily-use audit (2026-04-21) surfaced 4 correctness issues blocking adoption as primary MCP memory daemon. All 4 are exploitable or loss-inducing at normal usage rates. Fix before v0.4 schema changes so invariants hold during migration.

---

## Decision

Ship v0.3.3 patch release with four targeted fixes. No schema change, no breaking API change, no new MCP tools. Blocked daily-use adoption resolved before ADR-010/011 schema work begins.

---

## Fix 1 — JSON Escape in Error Responses (HIGH, Security)

**Site:** Every tool handler in `src/server.rs` (~22 sites).

**Current:**
```rust
format!("{{\"error\": \"{}\"}}", e.client_message())
```
User input containing `"` or `\n` corrupts the JSON-RPC frame — unescaped string interpolation into a JSON value.

**Reproducer:** `kb_drill({"id":"x\",\"ok\":true}"})` → smuggled fields appear in response.

**Fix:**
```rust
serde_json::json!({"error": e.client_message()}).to_string()
```
Applied at all ~22 handler sites. `serde_json` handles escaping unconditionally.

---

## Fix 2 — kb_consolidate Atomic Transaction (HIGH, Concurrency)

**Site:** `src/tools/consolidate.rs:42-67`.

**Current:** CREATE consolidated memory, then loop (archive source + RELATE) with no BEGIN/COMMIT. Any interruption mid-sequence leaves a new memory without archives, or archives without the consolidated memory.

**Fix:** Wrap the full sequence in a SurrealQL transaction:
```sql
BEGIN TRANSACTION;
  CREATE consolidated_memory ...;
  UPDATE source_1 SET status = 'archived';
  UPDATE source_2 SET status = 'archived';
  RELATE consolidated -[derives]-> source_1;
  RELATE consolidated -[derives]-> source_2;
COMMIT TRANSACTION;
```

**Failure mode fixed:** Ctrl-C mid-consolidate leaves inconsistent state (new memory + partial archives). With the transaction, the DB either reflects the full consolidation or none of it.

**Bonus:** Reject `ids.len() > 50` with `InvalidInput` — prevents unbounded transaction size.

---

## Fix 3 — stdio Shutdown Drain + JoinSet (MED, Concurrency)

**Sites:**
- `src/main.rs:133-145` — stdio transport path has no `ctrl_c` handler
- `src/tools/remember.rs` — post-create spawns detached `tokio::spawn` tasks (chunk writes, auto-link, entity link)
- Periodic prune/consolidation loops — untracked, not cancelled on shutdown

**Current:** `tokio::spawn` tasks are fire-and-forget. stdio disconnect drops in-flight post-create work silently. Laptop close after `kb_remember` loses chunk rows and graph edges.

**Fix:**
- Add `JoinSet` field on `KbServer` for post-create tasks; all post-create spawns go into the set.
- stdio run loop: `tokio::select!` on `tokio::signal::ctrl_c()` + service future.
- On shutdown signal: bounded 5-second drain of `JoinSet` before returning. Tasks that exceed the drain window are logged (IDs only, not content) and abandoned — partial writes are acceptable; silent data loss is not.
- Periodic tasks (prune, consolidation loop): `CancellationToken` from `tokio-util`; main loop cancels and awaits on shutdown.

**Failure mode fixed:** Post-create hooks for chunking, auto-link, and entity matching are no longer silently abandoned on disconnect.

---

## Fix 4 — kb_drill Visibility Filter (MED, Security)

**Site:** `src/db/memory.rs::get` (lines 115-136).

**Current:** Fetches the memory record by primary key only. No visibility check. A sensitive memory in project B is readable from any context if the ID leaks (via recall results, spreading activation outputs, or consolidation candidates).

**Fix:** Apply the same visibility semantics as `kb_recall`:
```sql
-- Default (no project context)
WHERE id = $id AND visibility != 'sensitive'

-- Within-project (project param provided)
WHERE id = $id
  AND (visibility != 'sensitive' OR project = $project)
```

This matches the recall predicate and closes the cross-project information leak.

---

## Test Additions

| Test | What it covers |
|------|----------------|
| JSON-escape fuzzer | Injects `"`, `\n`, `\\` into error-path inputs, asserts valid JSON response |
| consolidate-txn-rollback | Simulates interruption mid-consolidate, asserts DB state is consistent |
| stdio-drain | Verifies post-create hooks complete under bounded-drain shutdown |
| drill-visibility | Confirms sensitive memory in project B is not returned from project A context |
| consolidate-ids-cap | Asserts `InvalidInput` when `ids.len() > 50` |

+10 tests total (5 new cases, each with positive + negative assertions).

---

## Consequences

### Positive
- JSON injection vector closed at all tool handler sites.
- Consolidation is all-or-nothing — no half-consolidated state possible.
- Post-create work (chunks, edges) survives normal shutdown sequences.
- `kb_drill` respects visibility — closes cross-project leak.

### Negative
- 5-second shutdown drain adds latency to clean exits when post-create work is in flight.
- `JoinSet` on `KbServer` adds a small amount of synchronization overhead per post-create task.

### Trade-offs accepted
- Drain window (5s) is arbitrary — chosen to cover typical chunk + embed time on commodity hardware. Tasks exceeding it are logged and dropped; correctness is eventual, not guaranteed on hard kill.
- `ids.len() > 50` cap is conservative — no known use case requires consolidating more than 50 memories in one call.

---

## Alternatives Considered

- **Global JSON serialization wrapper** — A single response-builder function would also fix Fix 1, but 22 targeted replacements are lower risk and easier to review. Wrapper can be added in a follow-up cleanup.
- **Optimistic locking for consolidate** — Reads version fields and retries on conflict. Overkill for single-writer personal daemon; a transaction is sufficient.
- **Unlimited drain window** — Blocks shutdown indefinitely if a post-create task hangs. Bounded window is the correct trade-off.

---

## Related

- ADR-010: Agent authorship model (scheduled after this patch)
- ADR-011: Bitemporal model (scheduled after ADR-010)
