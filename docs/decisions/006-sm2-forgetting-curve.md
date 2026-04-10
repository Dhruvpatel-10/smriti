# ADR-006: SM-2 Spaced Repetition for Memory Decay

**Status:** Accepted  
**Date:** 2026-04-10  
**Context:** The original linear decay (`importance -= 1` weekly) was too crude. Important memories that happen to not be accessed for a week would decay at the same rate as genuinely forgotten ones. Research shows memory retention follows an exponential curve (Ebbinghaus, 1885), and spaced repetition (SM-2 algorithm) is the most effective schedule for long-term retention.

## Decision

Replace linear access decay with SM-2-inspired exponential scheduling.

## How It Works

### On Recall (successful access)

When a memory is returned by `kb_recall`:
- `ease_factor += 0.1` (capped at 4.0) — gets easier to remember
- `review_interval = ceil(review_interval * ease_factor)` — next review is further out
- `next_review_at = now + review_interval days`
- `access_count += 1`

This means: memories accessed frequently develop longer and longer intervals between expected reviews. A memory accessed 5 times might have a 60-day interval.

### On Decay (missed review)

The pruning job checks for memories where `next_review_at < now AND access_count = 0`:
- `importance -= 1` — losing relevance
- `ease_factor -= 0.2` (floored at 1.3) — harder to recall = faster future decay
- `review_interval = 1` — reset schedule, give it another chance
- `next_review_at = now + 1 day`

### Legacy Fallback

Memories created before migration 004 (no SM-2 fields) fall back to the original interval-based decay: importance reduced if `updated_at` is older than `access_decay_interval_days` and `access_count = 0`.

## Schema (Migration 004)

```sql
DEFINE FIELD IF NOT EXISTS ease_factor     ON TABLE memory TYPE option<float>;
DEFINE FIELD IF NOT EXISTS review_interval ON TABLE memory TYPE option<int>;
DEFINE FIELD IF NOT EXISTS next_review_at  ON TABLE memory TYPE option<datetime>;
```

## SM-2 Defaults

| Field | Initial Value | After 1st Access | After Miss |
|-------|--------------|-------------------|------------|
| ease_factor | 2.5 | 2.6 | max(ef - 0.2, 1.3) |
| review_interval | 1 day | ceil(1 * 2.6) = 3 days | 1 day |
| next_review_at | None | now + 3d | now + 1d |

## Why Not Full SM-2

Full SM-2 requires user quality ratings (0-5) per recall. In our system, there's no explicit "how hard was this to recall" signal. We use a binary signal: accessed (successful) or not accessed by review date (missed). This is simpler and appropriate for an automated system.

## Consequences

- Frequently accessed memories develop long review intervals (months).
- Rarely accessed memories decay faster (exponential, not linear).
- The `archive_below_threshold` step catches memories that fall below `importance_archive_threshold`.
- Never hard-deleted — archived memories stay queryable for history.
