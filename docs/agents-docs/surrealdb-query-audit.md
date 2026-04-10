# SurrealDB Query Audit

Audited: 2026-04-10
Scope: All `.rs` files under `src/db/` and `src/tools/` containing SurrealQL queries.

## Summary

18 source files audited. 157 SurrealQL query sites identified. Overall the codebase follows good practices: parameterized bindings are used consistently, RELATE idempotency is handled via pre-check + unique indexes, and status/visibility filters are applied correctly across query paths. However, there are several issues ranging from N+1 query patterns to potential silent failures and a full-table-scan concern in auto-linking.

**Critical: 0 | High: 3 | Medium: 7 | Low: 4 | Info: 3**

## Findings

### [high] src/db/search.rs:66-79 — N+1 SM-2 update loop in spawned task

`spawn_sm2_access_update()` fires individual `UPDATE` queries per memory inside a loop. For a recall returning 25 results, this issues 25 sequential queries. Should be batched into a single `UPDATE ... WHERE record::id(id) IN $ids` with computed values, or at minimum use a multi-statement batch.

```rust
for u in updates {
    let _ = db.query("UPDATE type::record('memory', $id) SET ...")
```

### [high] src/db/auto_link.rs:74-88 — N+1 project check inside similarity loop

`check_same_project()` issues a SELECT per similar memory inside the `auto_link_similar` loop. With up to 15 candidates from `find_similar`, this fires 15 individual queries. The project field is already selected in `dedup::find_similar` (line 93: `SELECT record::id(id) AS mem_id, project, ...`) but the returned `project` is never used. Fix: return project from `find_similar` and check in-memory.

### [high] src/db/auto_link.rs:100-102 — Full table scan on entity for name matching

`match_known_entities()` runs `SELECT name FROM entity` with no filtering, loading every entity into memory on every memory creation. As the entity table grows, this becomes a bottleneck. Should use a SurrealQL approach (e.g., `SELECT name FROM entity WHERE string::lowercase(name) INSIDE string::lowercase($summary)`) or cache entity names in-process.

### [medium] src/db/memory.rs:112-120 — UPDATE without verifying result in get_and_touch

`get_and_touch()` issues an UPDATE to increment access_count but discards the response without checking whether the update succeeded or the record exists. If the record was archived between the UPDATE and the subsequent `get()`, the user gets a NotFound error without knowing the touch failed silently. Should consume the result and check for empty response.

```rust
db.query("UPDATE type::record('memory', $id) SET access_count += 1, ...")
    // .await result is discarded via map_err only — no row check
```

### [medium] src/db/memory.rs:146-153 — UPDATE without verifying result in supersede

`supersede()` does not check whether the UPDATE actually affected a row. If the memory ID doesn't exist, the UPDATE silently succeeds with no rows. The caller (`remember.rs:169`) does pre-check via `memory::get()`, but the function contract is misleading — it should return NotFound on missing ID like `archive()` does.

### [medium] src/db/memory.rs:161-171 — UPDATE without verifying result in promote

Same pattern as `supersede()` — UPDATE discards the result without checking if any row was actually modified. The function pre-fetches via `get()`, so in practice the record will exist, but if another concurrent operation archives it between the get and update, the promote silently does nothing.

### [medium] src/db/search.rs:370-380 — format! interpolation for vector query filters

The vector search SQL is constructed via `format!()` with interpolated filter clauses. While the interpolated strings are hardcoded string literals (not user input), this pattern is fragile — any future edit that introduces a variable into these fragments would create an injection path. Consider using the same conditional bind + `conditions.join()` pattern used for BM25 (lines 286-312) for consistency and safety.

```rust
let vector_sql = format!(
    "SELECT ... FROM memory WHERE ...{visibility_filter}{type_filter}...",
    type_filter = if memory_type.is_some() { " AND memory_type = $memory_type" } else { "" },
```

### [medium] src/db/prune.rs:117-133 — format! interpolation of immunity_condition()

`immunity_condition()` builds a SQL fragment with hardcoded subtype names interpolated via `format!()`. The values come from `IMMUNE_SUBTYPES` constant (safe), but the pattern bypasses parameterized bindings. If someone adds a subtype with a quote in the name (unlikely but possible), this would break. More importantly, the `IMPORTANCE_IMMUNE_THRESHOLD` constant is interpolated as a raw integer. Consider binding these as parameters.

```rust
fn immunity_condition() -> String {
    format!("AND importance < {IMPORTANCE_IMMUNE_THRESHOLD} AND subtype NOT IN [{subtypes}]")
}
```

### [medium] src/db/search.rs:438-439 — string::lowercase CONTAINS for person matching is not exact match

The entity-anchored query uses `string::lowercase(name) CONTAINS string::lowercase($name)`. `CONTAINS` is a substring check in SurrealQL, not equality. Searching for person "Al" would match "Alice", "Albert", "Hal", etc. Should use `=` with `string::lowercase()` on both sides, or a more precise matching strategy.

### [medium] src/db/consolidation.rs:81-88 — Full table scan with LIMIT 200 for graph clusters

The graph cluster query scans up to 200 active memories with `array::group(->relates->memory.id)`, which triggers a graph traversal per row. For a KB with many memories, this is expensive. There's no index on the `relates` edge that would help filter this. Consider restricting to memories with known relates edges or using a more targeted subquery.

### [low] src/db/reembed.rs:60-91 — N+1 embed+store in reembed loop

`reembed_all()` processes memories one at a time in a loop, issuing individual UPDATE queries per memory. For a large reembed operation, this could be slow. However, since embedding is CPU-bound and sequential by nature (model inference), the DB update is not the bottleneck. Noted for awareness — batching the UPDATEs would provide marginal improvement.

### [low] src/db/export.rs:29-33 — Export queries lack ORDER BY

`export_all()` queries `SELECT * FROM memory` and `SELECT * FROM entity` without ORDER BY. This means export order is nondeterministic, making it harder to diff exports across runs. Consider adding `ORDER BY created_at ASC` for reproducibility.

### [low] src/db/search.rs:148 — record::id(id) IN $ids may not use index

In spreading activation, `WHERE record::id(id) IN $ids` applies a function to the id field before comparison, which prevents index usage on the primary key. SurrealDB may still optimize this internally for record IDs, but the pattern is used in multiple places (lines 148, 215, 517) and should be verified against the query planner.

### [low] src/db/memory.rs:71-72 — serde_json::to_value for enum bindings

`memory::create()` uses `serde_json::to_value(memory_type).unwrap()` to bind enum values. This works because the enums serialize to strings, but it adds unnecessary allocation. The Surreal SDK should accept the enum directly if it implements `Serialize`. This is a minor performance nit, not a correctness issue.

### [info] src/db/graph.rs:32-34 — format! for edge table name is safe

The `link_memories()` function interpolates `{table}` into RELATE SQL. This is safe because `table` derives from `RelationType::to_string()`, which is a Rust enum — not user input. The comment at line 22 correctly documents this decision.

### [info] src/db/dedup.rs:38-46 — Vector binding as Vec<f32> is correct

`check_duplicate()` and `find_similar()` bind the embedding vector as `embedding.to_vec()` (Vec<f32>). This is correct — the Surreal Rust SDK serializes Vec<f32> to a JSON array of numbers, which SurrealDB's `vector::similarity::cosine()` accepts. The schema defines `vector` as `option<array<float>>` which matches.

### [info] src/db/search.rs:286-296 — Visibility filtering is consistent and correct

All recall paths (BM25, vector, spreading activation, entity-anchored fetch, status query, markdown summary) correctly apply visibility filtering:
- Project-scoped queries exclude `sensitive` only
- Cross-project queries restrict to `standard` only
- Status queries exclude `sensitive`
- Export optionally excludes `sensitive`

Status filter (`WHERE status = 'active'`) is applied in all query paths that return memories to users. Verified in: search.rs (BM25, vector, spreading activation neighbor fetch, entity-anchored), consolidation.rs, alerts.rs, prune.rs, markdown.rs, entity.rs (recent_memories, count_memories, find).

## Recommendations

1. **Batch the SM-2 updates** (high ROI). Collect all SM-2 update data in Rust, then issue a single multi-statement query or use `UPDATE ... WHERE record::id(id) IN $ids` with a MERGE pattern.

2. **Use the already-fetched project field from find_similar** to eliminate the N+1 in auto_link.rs. The `project` column is already SELECTed in dedup.rs:93 but dropped during result parsing.

3. **Cache entity names** in-process (e.g., refresh every 60s) to avoid the full table scan in `match_known_entities()`. The entity table changes infrequently.

4. **Unify the SQL-building pattern** in search.rs. The BM25 path uses conditional push to a `conditions` vec (safe), while the vector path uses `format!()` interpolation of filter fragments (fragile). Standardize on the conditions-vec approach.

5. **Add RETURN BEFORE/AFTER to UPDATE calls** in `supersede()` and `promote()` and check for empty results to detect missing records, matching the pattern already used in `archive()`.

6. **Fix the CONTAINS substring match** in entity-anchored person filtering (search.rs:438) to use exact case-insensitive equality instead of substring containment.

7. **Verify record::id(id) IN $ids performance** with SurrealDB's query planner. If it causes full scans, consider using `id IN [record-id-array]` with pre-constructed RecordId bindings instead.
