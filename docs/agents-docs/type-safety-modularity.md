# Type Safety & Modularity Audit

## Summary

The codebase is well-structured with a clean type system. The major categories of issues are:
1. **Duplicate TTL logic** — two functions compute the same table, one of which goes unused in the primary write path.
2. **Panic-capable `unwrap()` calls** in production code paths (not tests), primarily in `server.rs` and `db/memory.rs`.
3. **Raw string literals** embedded in SurrealQL for enum values instead of using the serialized form from the existing enum impls — creates silent drift risk.
4. **Lossy as-casts** that could silently truncate or overflow under adversarial or large inputs.
5. **Untyped `String` fields** where typed newtypes or enums already exist.
6. **Error-swallowing** patterns that hide real failures in several DB helper paths.
7. **`Display` for `MemorySubtype`/`RelationType`** uses `serde_json` round-trip — fragile.

---

## Findings

### [HIGH] src/db/validation.rs:55–83 — Duplicate TTL logic, `enforce_ttl` bypassed by `memory::create`

`validation.rs` exposes two functions with identical match arms:
- `enforce_ttl(type, subtype) -> Option<DateTime<Utc>>` — absolute timestamp
- `ttl_duration(type, subtype) -> Option<Duration>` — relative duration

`tools/remember.rs:72` calls `enforce_ttl` and passes the result to `memory::create` as the `expires_at` parameter. However, `memory::create` (line 41) ignores the passed `expires_at` and recomputes from `ttl_duration` as a fallback via `expires_at.or_else(...)`. The net result is that the absolute timestamp from `enforce_ttl` is used (because `or_else` only fires on `None`), but the two functions compute independently. If one is updated and the other is not, TTLs silently diverge. CLAUDE.md explicitly states "Single TTL source: `validation::ttl_duration()`" — yet `enforce_ttl` is the one actually called in the write path. The docstring on `memory::create` says "single source: validation::ttl_duration" but the caller uses `enforce_ttl`. The two sources should be collapsed into one.

### [HIGH] src/server.rs:69,78,87,96,105,114,125,134,143,154,163,172,183,192,208,217,226,271 — `serde_json::to_string(&out).unwrap()` can panic

Every tool handler in `server.rs` serializes its output with `.unwrap()`. While `serde_json::to_string` on typed structs rarely fails, it _can_ for types containing `f64::NAN`, `f64::INFINITY`, or certain `serde_json::Value` nodes. If any output struct ever transitively holds a non-finite float (e.g., from a cosine score that becomes NaN), the server panics and drops the MCP connection. These should use `.unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}}}"))`  or a helper that returns a proper error JSON string.

### [HIGH] src/db/memory.rs:71–73,78 — `serde_json::to_value(enum).unwrap()` in write path

Four `.unwrap()` calls bind `MemoryType`, `MemorySubtype`, `Source`, and `Visibility` to SurrealQL parameters via `serde_json::to_value()`. These enums implement `Serialize` cleanly and will not actually fail today, but this pattern is fragile: if any enum variant is ever changed to a type that can't serialize (e.g., adding a non-serializable field), this becomes a silent panic in the middle of a DB write. The correct pattern is to propagate `serde_json::to_value(...)` with `?` or use the `Display` impl (since snake_case strings are also valid SurrealDB values for string-typed fields).

### [HIGH] src/db/memory.rs:178 — `serde_json::to_value(status).unwrap()` in `count()`

Same class as above, same fix.

### [HIGH] src/db/search.rs:335,386 — `serde_json::to_value(memory_type).unwrap()` inside query builder

Two `.unwrap()` calls inside the hot `recall()` path. A panic here brings down an entire recall operation.

### [MEDIUM] src/types/enums.rs:79–84, 202–207 — `Display` for `MemorySubtype` and `RelationType` via `serde_json` round-trip

Both `fmt::Display` impls serialize `self` to JSON with `serde_json::to_value(self).ok().and_then(...)` and then fall back to empty string via `unwrap_or_default()` on a serialization that cannot actually fail. This is:
1. Unnecessarily expensive (JSON allocation for a simple string conversion).
2. Silently returns an empty string if serialization ever fails, making log messages and SurrealQL string interpolation (e.g., in `graph.rs:19`) silently wrong.

`MemoryType` uses a proper `match` for `Display` — `MemorySubtype` and `RelationType` should do the same. `RelationType::to_string()` is used directly in SurrealQL table name interpolation (`graph.rs:19,32`); an empty string would produce invalid SQL with no error.

### [MEDIUM] src/types/alert.rs:32 — `Alert.reference_id: String` instead of a typed ID

`Alert.reference_id` is used to hold either a `MemoryId` or an `EntityId` string (or even a raw entity `name` string in the stale-contact case — see `alerts.rs:151`). This conflates three different concepts in one untyped field with no documentation of which callers store which kind of value. A caller reading an alert cannot know whether to call `kb_drill(id)` or `kb_entity_get(id)`. Consider an enum `AlertReference { Memory(MemoryId), Entity(EntityId), PersonName(String) }`.

### [MEDIUM] src/db/search.rs:107 — `as i64` cast on SM-2 interval could overflow

```rust
let new_ri = ((ri as f64) * ef).ceil().max(1.0) as i64;
```

`ri` starts as 1, `ef` starts at 2.5 and is capped at 4.0. After ~62 successful recalls, `ri` reaches `2^62` (overflows i64) and the cast is undefined behavior in debug mode and wraps in release mode. With `ef=4.0` and starting at 1, the value exceeds i64::MAX after about 32 review cycles. This should be clamped before casting: `.min(i64::MAX as f64) as i64`, or the interval field should be stored as f64.

### [MEDIUM] src/db/search.rs:154,224,403,527 and multiple other sites — `.unwrap_or_default()` after `.take()` silently drops DB errors

Pattern:
```rust
let rows: Vec<serde_json::Value> = result.take(0).unwrap_or_default();
```

`result.take(0)` returns `Result<T, E>`. `.unwrap_or_default()` silently treats a DB deserialization error as an empty result set. This hides real issues like schema mismatches or SurrealDB returning an unexpected type. In production, a silently-empty recall is worse than a visible error. These should use `.map_err(|e| KbError::db(context, e))?` like the rest of the codebase already does correctly in many places (e.g., `memory.rs:102-103`). The inconsistency suggests these were added hastily.

Affected locations: `search.rs:154`, `search.rs:224`, `search.rs:403`, `search.rs:527`, `search.rs:710-714`, `prune.rs:72`, `prune.rs:168`, `prune.rs:198`, `consolidation.rs:56`, `consolidation.rs:92`, `reembed.rs:54`, `markdown.rs:162`.

### [MEDIUM] src/db/search.rs:521 — `as i64` cast after `saturating_sub` on `usize`

```rust
let extra_limit = (limit_usize.saturating_sub(text_ranked.len())) as i64;
```

`limit_usize` is at most 25 (u8 clamped), so this cast is safe in practice. However it is not obviously safe and should either use `i64::try_from(...).unwrap_or(0)` or be documented.

### [MEDIUM] src/db/prune.rs:11 — Raw string literals for immune subtypes, not derived from enum

```rust
const IMMUNE_SUBTYPES: &[&str] = &["decision", "value", "life_goal"];
```

These strings are embedded directly in `immunity_condition()` which interpolates them into SurrealQL. If `MemorySubtype::Decision` is renamed or the serialized form changes, this array silently goes stale. It should be derived from `MemorySubtype::Decision.to_string()` etc. or from an `impl` method on the enum.

### [MEDIUM] src/db/search.rs:286–295 — Raw string literals for enum values in dynamic SurrealQL conditions

The BM25 query in `recall()` builds conditions like:
```rust
conditions.push("status = 'active'".to_string());
conditions.push("visibility = 'standard'".to_string());
```

These are raw string literals that must match the serde `snake_case` output of `MemoryStatus` and `Visibility`. Same risk as the prune immunity array: a rename of a variant breaks these queries silently (data returned when it should be filtered or vice versa). Similarly in `alerts.rs:33-35` (`memory_type = 'prospective'`, `subtype = 'action_item'`), `status_query()` in `search.rs:677-685`, etc.

The safest fix is a small `sql_val(enum)` helper that calls `serde_json::to_value` once and converts to a SQL literal string, keeping a single serialization codepath.

### [MEDIUM] src/db/graph.rs:19,32 — SurrealQL table name interpolated from `RelationType::to_string()`

`link_memories` uses `relation.to_string()` as a SurrealQL table identifier:
```rust
let table = relation.to_string();
let sql = format!("LET $existing = ... FROM {table} WHERE ...");
```

This is called out in a comment as acceptable because `table` derives from a safe enum. However, because `RelationType`'s `Display` uses the fragile `serde_json` round-trip (see finding above), if `Display` ever returns an empty string, this produces invalid SurrealQL (`FROM  WHERE`) that would fail at runtime. Fixing `RelationType::Display` to use a `match` (see above) removes this risk entirely.

### [LOW] src/types/export.rs:4 — `ExportEntry.entry_type: String` instead of an enum

`ExportEntry` tags each exported line with `entry_type: "memory"` or `"entity"`. On import (`export.rs:136`), `entry_type.as_str()` is matched with a string match. An `enum ExportEntryType { Memory, Entity }` would make the match exhaustive and enable the compiler to catch any new table types added in future migrations.

### [LOW] src/types/entity.rs:13,23 — `Entity.kind: String` and `EntitySummary.kind: String` — no validation

`kind` accepts any string (`"person"`, `"company"`, `"product"`, `"concept"`, `"project"` per the tool docs). The tool handler in `entity.rs` passes this through without validation. An unrecognized kind is silently stored and will later appear in `kb_people` or `kb_entity_find` results with no error. A `EntityKind` enum or at minimum a validation call would catch typos from callers.

### [LOW] src/db/alerts.rs:140-145 — `last_contact` parsed from string rather than typed datetime field

`check_stale_contacts` fetches `attrs.last_contact` and parses it as a string:
```rust
let last_contact_str = row.get("last_contact").and_then(|v| v.as_str());
if let Some(lc) = last_contact_str && let Ok(last_contact) = lc.parse::<chrono::DateTime<Utc>>()
```

`last_contact` is stored by `graph::link_entity` as a string (`now_str = now.to_rfc3339()`) rather than a SurrealDB datetime. Any entity whose `attrs` was set via `kb_entity_upsert` with a datetime-typed attrs value will have a different format and silently return no stale contact alerts. The field should be stored as a native `DateTime<Utc>` and bound directly, not stringified.

### [LOW] src/server.rs:253 — `u8 as usize` cast for limit

```rust
let limit = params.0.limit.unwrap_or(10).min(50) as usize;
```

`u8` is always in range for `usize` on all supported platforms. Harmless but inconsistent with places that use `.min(25) as i64` for DB binding. Minor: should be consistent.

### [LOW] src/config.rs:124 — `std::env::var("HOME").unwrap()` panics if HOME is unset

`resolve_path` calls `std::env::var("HOME").unwrap()` when a path starts with `~/`. If the server is launched in an environment without `HOME` set (systemd service, container, CI), this panics at startup. Should use `std::env::var("HOME").unwrap_or_else(|_| "/root".to_string())` or return a `Result`.

### [LOW] src/db/embeddings.rs:93 — `.expect("model should be initialized")` inside `spawn_blocking`

```rust
let loaded = model_arc.get().expect("model should be initialized");
```

`ensure_loaded()` is called before this closure, so the `OnceLock` is guaranteed to be set. The `expect` is logically safe but still a panic-path. In an async context, a panic inside `spawn_blocking` propagates as a `JoinError` which is then mapped to `KbError::Embedding`. Acceptable, but could be replaced with `ok_or_else(|| KbError::Embedding("model not initialized".into()))?` for consistency with the rest of the error handling.

### [INFO] src/db/memory.rs:41 and src/tools/remember.rs:72 — TTL computed twice for same memory

`remember.rs` calls `validation::enforce_ttl` (absolute timestamp) and passes the result as `expires_at` to `memory::create`. `memory::create` then does:
```rust
let expires = expires_at.or_else(|| validation::ttl_duration(memory_type, subtype).map(|d| now + d));
```

Because `enforce_ttl` returns `Some(...)` for working/* subtypes, `ttl_duration` is never reached in those cases — the `or_else` is dead code in the write path. But for direct callers of `memory::create` that pass `expires_at = None` (e.g., `consolidate.rs:48`), `ttl_duration` IS used as the fallback, which is correct behavior. The two paths compute the same result but through different functions. This is not a bug, but it is confusing and violates the "single source" invariant stated in CLAUDE.md.

---

## Recommendations

1. **Collapse TTL duplication**: Delete `enforce_ttl` from `validation.rs`. Have `remember.rs` call `ttl_duration` and compute the absolute timestamp inline (`Utc::now() + d`). Update CLAUDE.md to reflect the single source. This resolves the INFO finding and clarifies the HIGH finding.

2. **Replace `serde_json::to_value(enum).unwrap()` with `?`-propagation or `Display`**: Everywhere an enum is bound to a SurrealDB query, either (a) use `serde_json::to_value(x).map_err(KbError::Json)?` or (b) use the `Display` impl directly (valid since all enum fields are stored as snake_case strings in SurrealDB). Option (b) requires fixing `MemorySubtype::Display` and `RelationType::Display` first (see below).

3. **Fix `Display` for `MemorySubtype` and `RelationType`**: Replace the `serde_json` round-trip with a `match` expression, identical in structure to `MemoryType::Display`. This makes the impl infallible and removes the empty-string fallback.

4. **Replace `.unwrap_or_default()` on DB `.take()` calls**: Use `.map_err(|e| KbError::db(context, e))?` consistently. Establish a lint or code review rule that `.unwrap_or_default()` on a `Result` is not permitted in production DB code.

5. **Clamp SM-2 review interval before `as i64` cast**: Add `.min(365 * 10)` (10 years in days) before the cast in `compute_sm2`. This is a reasonable upper bound for any spaced-repetition interval and prevents the overflow.

6. **Type the `IMMUNE_SUBTYPES` array in `prune.rs`**: Use `MemorySubtype` values and their `Display` output rather than hardcoded strings.

7. **Type `Alert.reference_id`**: Introduce `enum AlertReference` or at minimum document the contract per `AlertType` variant.

8. **Validate `entity kind`** in `entity::upsert` against an allowlist or enum.

9. **Store `last_contact` as a native datetime**: In `graph::link_entity`, bind `now` (a `DateTime<Utc>`) directly rather than converting to a string, so SurrealDB stores it as a datetime type and the alert query can compare it correctly.

10. **Guard `HOME` env var lookup in `config.rs`** to avoid startup panic in headless environments.
