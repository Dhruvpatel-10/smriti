# ADR-003: Numbered Schema Migration System

**Status:** Accepted  
**Date:** 2026-04-08  
**Context:** The initial `init_schema()` was a monolithic SQL blob. No way to evolve the schema (add fields, indexes) without manual intervention or risking data loss on upgrade.

## Decision

Replace `init_schema()` with a versioned migration system tracked in a `_migration` table.

## Implementation

- `src/db/migrations.rs`: `MIGRATIONS` const array of `(version, description, sql)`.
- `run(db)` on every startup: ensures `_migration` table exists, gets current version, applies pending migrations in order.
- Each migration is idempotent (`IF NOT EXISTS` everywhere).
- Rule: **never edit an existing migration. Add a new one.**

## Current Migrations

| Version | Description |
|---------|-------------|
| 1 | Initial schema: memory, entity, person, edge tables, FTS indexes |
| 2 | UNIQUE indexes on all 6 edge tables (prevents duplicate RELATE) |
| 3 | Vector embedding field (`option<array<float>>`) |
| 4 | SM-2 spaced repetition fields (ease_factor, review_interval, next_review_at) |

## Rationale

- Safe upgrades: binary ships with all migrations, applies only what's new.
- Rollback story: migrations are additive (new fields, new indexes). Old data isn't touched.
- Auditability: `_migration` table shows exactly what schema version is running.

## Consequences

- Schema changes require adding a new migration entry.
- All migrations must be idempotent (safe to re-run).
- `latest_version()` reported in startup logs for debugging.
