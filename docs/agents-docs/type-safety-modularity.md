# Type Safety & Modularity Audit

**Date:** 2026-04-10
**Scope:** All `src/` files
**Auditor:** Claude Sonnet 4.6 (automated)

---

## Summary

Well-structured type system. Major issue categories were: duplicate TTL logic, panic-capable `unwrap()` in production paths, raw string literals for enum values in SurrealQL (silent drift risk), lossy as-casts, untyped String fields where typed newtypes existed, and fragile `Display` via `serde_json` round-trip. All HIGH and MEDIUM findings closed. One LOW finding (harmless cast) remains open.

_As of 2026-04-20: all HIGH findings closed; all MEDIUM findings closed; all other LOW findings closed. One LOW finding open._

---

## Audit Rubric

When reviewing type safety:
1. No bare `unwrap()` / `expect()` in production code paths (server.rs, db/, tools/). Tests exempt.
2. No raw string literals for enum values in SurrealQL — use `as_str()` const fn or bound via `.bind()`.
3. No `serde_json::to_value(enum).unwrap()` in hot paths — use `as_str()` or propagate with `?`.
4. All newtypes used consistently — no raw `String` IDs where `MemoryId`/`EntityId` are expected.
5. `as` numeric casts must be checked for overflow or documented safe with invariant comment.
6. Single TTL source: `validation::ttl_duration()` only.

---

## Open Finding

### [LOW] src/server.rs `kb_consolidation_candidates` — `u8` limit conversion

```rust
let limit = usize::from(params.0.limit.unwrap_or(10).min(50));
```

`u8` is always in range for `usize` on all supported platforms — harmless. The code now uses `usize::from()` (infallible, documented safe conversion) rather than `as usize`. Minor stylistic finding; effectively resolved by the refactor.

---

## Closed Findings

### [CLOSED] src/db/validation.rs:55-83 — Duplicate TTL logic
**Closed 2026-04-20 — `enforce_ttl()` removed; `ttl_duration()` in `validation.rs` is the single source per CLAUDE.md invariant. `remember.rs` computes absolute timestamp inline; `memory::create()` uses `ttl_duration()` as the fallback for direct callers.**

### [CLOSED] src/server.rs — `serde_json::to_string(&out).unwrap()` can panic
**Closed 2026-04-20 — No bare `.unwrap()` on serialization in current server.rs; all handlers use `unwrap_or_else`.**

### [CLOSED] src/db/memory.rs:71-73,78 — `serde_json::to_value(enum).unwrap()` in write path
**Closed 2026-04-20 — No `serde_json::to_value` unwraps found in current memory.rs; enums bound via `as_str()` or propagated `?`.**

### [CLOSED] src/db/search.rs:335,386 — `serde_json::to_value(memory_type).unwrap()` inside query builder
**Closed 2026-04-20 — Not found in current source; eliminated alongside the enum binding pass.**

### [CLOSED] src/types/enums.rs:79-84, 202-207 — `Display` for `MemorySubtype` and `RelationType` via `serde_json` round-trip
**Closed 2026-04-20 — `as_str() -> &'static str` const fn added to all enums; `Display` delegates to `as_str()`. Eliminates JSON allocation and the empty-string silent failure on serialization error. 4 parity tests verify `as_str()` output matches `serde_json::to_value` for all variants.**

### [CLOSED] src/types/alert.rs:32 — `Alert.reference_id: String` instead of a typed ID
**Closed 2026-04-20 — `AlertReference { Memory(MemoryId), Entity(EntityId), PersonName(String) }` enum introduced; all alert construction sites updated; callers can pattern-match to determine whether to call `kb_drill` or `kb_entity_get`.**

### [CLOSED] src/db/search.rs:107 — `as i64` cast on SM-2 interval could overflow
**Closed 2026-04-20 — `MAX_INTERVAL_DAYS = 365.0` cap applied before the cast; overflow path eliminated.**

### [CLOSED] src/db/search.rs:154,224,403,527 and others — `.unwrap_or_default()` after `.take()` silently drops DB errors
**Closed 2026-04-20 — All affected sites (`search.rs`, `prune.rs`, `consolidation.rs`, `reembed.rs`, `markdown.rs`) now use `KbError::db` propagation or `tracing::warn!` with empty-vec fallback.**

### [CLOSED] src/db/search.rs:521 — `as i64` cast after `saturating_sub` on `usize`
**Closed 2026-04-20 — Safe by analysis: `limit_usize` is clamped to ≤25 before this point; cast documented with invariant comment.**

### [CLOSED] src/db/prune.rs:11 — Raw string literals for immune subtypes, not derived from enum
**Closed 2026-04-20 — `MemorySubtype::immune_strs()` method added; values bound via `.bind()`, no interpolation.**

### [CLOSED] src/db/search.rs:286-295 — Raw string literals for enum values in dynamic SurrealQL conditions
**Closed 2026-04-20 — All five enum types now have `as_str() -> &'static str` const fn; `Display` delegates to `as_str()`; 4 parity tests verify correctness.**

### [CLOSED] src/db/graph.rs:19,32 — SurrealQL table name interpolated from `RelationType::to_string()`
**Closed 2026-04-20 — `RelationType::Display` delegates to `as_str()` (match-based, infallible); empty-string risk from old `serde_json` round-trip eliminated.**

### [CLOSED] src/types/export.rs:4 — `ExportEntry.entry_type: String` instead of an enum
**Closed 2026-04-20 — `ExportEntryType { Memory, Entity }` enum introduced with exhaustive match on import; compiler now catches any new table types added in future migrations.**

### [CLOSED] src/types/entity.rs:13,23 — `Entity.kind: String` and `EntitySummary.kind: String` — no validation
**Closed 2026-04-20 — `EntityKind` enum added with `FromStr` validation in the entity upsert path; unrecognized kind values now rejected at the tool layer.**

### [CLOSED] src/db/alerts.rs:140-145 — `last_contact` parsed from string rather than typed datetime field
**Closed 2026-04-20 — `last_contact` now stored as SurrealDB native datetime and parsed directly as `DateTime<Utc>`; the old string parsing path that silently returned no stale alerts for RFC3339-formatted values is eliminated.**

### [CLOSED] src/config.rs:124 — `std::env::var("HOME").unwrap()` panics if HOME is unset
**Closed 2026-04-20 — `resolve_path` now returns `Result`; HOME lookup uses `ok_or_else` and propagates cleanly on missing env var.**

### [CLOSED] src/db/embeddings.rs:93 — `.expect("model should be initialized")` inside `spawn_blocking`
**Closed 2026-04-20 — Replaced with `ok_or_else(|| KbError::Embedding("model not initialized".into()))?`; panic path eliminated.**

---

## Info Finding (no action needed)

### [INFO] src/db/memory.rs:41 and src/tools/remember.rs:72 — TTL computed in two places
`remember.rs` calls `validation::ttl_duration()` to compute an absolute timestamp, which is passed to `memory::create()` as `expires_at`. Inside `memory::create()`, `ttl_duration()` is called again as a fallback for `None` values (from direct callers like `consolidate.rs`). The two paths compute the same result via the same function — not a bug, but the `or_else` branch is dead code for all writes that go through `remember.rs`. Not worth fixing; the CLAUDE.md invariant (single source at `validation::ttl_duration`) is satisfied.
