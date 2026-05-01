# SurrealDB v3 Notes

Dev reference for SurrealDB v3 gotchas encountered during development.

## Engine
- `surrealdb::engine::local::SurrealKV` for embedded, no-daemon operation
- Crate: `surrealdb 3` with `default-features = false, features = ["kv-surrealkv"]`
- SurrealKV releases file locks on background threads — allow delay after drop before reconnect

## Query Syntax
- `type::record()` for SELECT/UPDATE/CREATE positions
- `RecordId::new()` bindings for RELATE subject/object
- RELATE edge table must be a static identifier — no bind-variable in the middle slot
- `IF/ELSE` uses curly-brace blocks `{ }`, not `THEN...END`
- `SELECT *, field AS alias` RENAMES the field — never alias fields you need to keep
- Graph traversal filters: `table[WHERE condition]`
- `value` is a **reserved keyword** (used in `SELECT VALUE` clause) — fields named `value` must be backtick-quoted: `` SELECT `value` FROM _kb_metadata `` and `` UPSERT _kb_metadata MERGE { ..., `value`: $value, ... } ``. The underscore-prefixed table name (`_kb_metadata`) is fine unquoted.

## Binding
- Bind `DateTime<Utc>` directly — `serde_json::to_value()` produces strings v3 won't coerce
- `.await.map_err()` on a query only catches transport errors
- SurrealQL-level errors are embedded in the `Response` and only surface on `.take()`

## Functions
- `COALESCE()` does NOT exist — use `??` (null coalescing operator)
- `math::max()` takes 1 arg (an array) — use `IF x > y { x } ELSE { y }` for two-value max

## Data Types
- `FULLTEXT ANALYZER ... BM25` and `@@` operator valid
- `INSERT INTO ... ON DUPLICATE KEY UPDATE` valid
- MTREE vector indexes NOT supported in SurrealKV — use brute-force `vector::similarity::cosine()`
- Vector field type: `option<array<float>>` — store as `Vec<f64>` from Rust
- `TYPE object FLEXIBLE DEFAULT {}` for freeform nested-key fields — without `FLEXIBLE`, SCHEMAFULL tables reject undeclared sub-keys

## Schema
- Defined in `src/db/migrations.rs`, versioned numbered migrations
- Idempotent with `IF NOT EXISTS`
- Never edit existing migrations — add new ones

## Known Function Gaps

- `array::group` is NOT available in SurrealKV embedded mode. The consolidation
  candidate heuristic-2 (graph-cluster detection) was rewritten to avoid it — uses
  explicit graph traversal with `->relates->` rather than a grouped aggregate. Do not
  use `array::group` anywhere in query code; it will silently return no rows or error.

- `HAVING` clause behaviour: SurrealDB v3 `HAVING` may not correctly filter on
  computed aggregate aliases in all contexts. Prefer `WHERE` on sub-selects or
  post-processing in Rust when filtering on aggregated values.
