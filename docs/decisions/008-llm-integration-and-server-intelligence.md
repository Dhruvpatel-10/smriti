# ADR-008: LLM Integration, Server-Side Intelligence, and Taxonomy Refinement

**Status:** Accepted + Amended  
**Date:** 2026-04-10  
**Amended:** 2026-04-10 (post-DA review — see Amendment section below)  
**Context:** Deep review of taxonomy, retrieval, graph, and decay against cognitive science and all major competitors (MemGPT/Letta, Zep/Graphiti, Mem0, Cognee, SuperMemory, LangMem). Devil's advocate review by simulated PhD memory systems expert.

---

## Amendment (2026-04-10): Post-DA Review Changes

After 3 rounds of review (codebase audit + research + Devil's Advocate), the following changes
were made to the original ADR-008 spec:

### Taxonomy: 28→23 subtypes (not 28→20 as DA suggested)
- **Removed (5):** Outcome→Reflection, Insight→Pattern, Assumption→Hypothesis, FollowUp→ActionItem, Commitment→ActionItem
- **Kept (DA wanted to remove but we kept):** DailyLog (distinct retrieval), Signal (accumulates→pattern), LifeGoal (distinct decay)

### Write Path: Embed-First, Create-Last
- Original: create→embed→dedup (broken: self-match bug, orphaned records on reject)
- Fixed: validate→embed→dedup→create (no orphaned records, no self-match)
- Contextual embedding: prepend `[type/subtype] [project] [domains]` before encoding
- Summary min: 20→10 chars. Dedup: 0.95/0.85→0.92/0.82.

### Retrieval: 7→5 stages
- **Dropped:** Intent routing (always balanced RRF), Lost-in-middle (belongs in client)
- **Added:** Relevance floor (cosine <0.30 excluded), spreading activation cap (3 max)

### Graph: Tighter thresholds
- Auto-link: 0.70→0.75 relates (0.85 cross-domain), 0.85→0.90 extends
- Edge cap: 8 similarity edges per memory
- Entity matching: substring→whole-word boundary, min name 4 chars

### Decay: Keep SM-2, fix immunity bypass
- DA suggested replacing SM-2 with recency-weighted — rejected (migration risk too high)
- Fixed: `archive_below_threshold` now respects immunity rules
- Access count updates ALL returned memories, not just BM25 rows

### Bug Fixes
- Stale contacts: `link_entity` now sets `attrs.last_contact`
- `link_assigned`: upserts entity first (was silently failing)
- Double-embed eliminated: pre-computed vector passed to auto_link
- Supersession: verifies old memory exists and is active before superseding
- TTL: single source of truth in `validation::ttl_duration()` (removed duplicate in memory.rs)

---

## Decision Summary

The system has a strong specification but incomplete implementation. The gap between spec and code is the primary risk. This ADR documents all findings and the build plan to close that gap.

---

## 1. LLM Integration: Rig Framework + Gemini + Groq

### Decision
Use the [Rig](https://docs.rig.rs) Rust framework for all LLM and embedding operations.

### Provider Strategy
| Role | Provider | Why |
|------|----------|-----|
| Primary LLM (drain session) | Google Gemini 2.0 Flash | Free tier: 1,500 req/day, native JSON schema enforcement |
| Fallback LLM | Groq (Llama 3.1 70B) | Burst overflow if Gemini rate-limited |
| Embeddings | rig-fastembed (local ONNX) | EmbeddingGemma-300M, in-process, zero latency |
| Vector store | rig-surrealdb | Bridges Rig embedding pipeline to SurrealDB |

### Architecture
```
Claude sessions → call MCP tools directly (kb_remember, kb_recall, etc.)
                  Claude does the reasoning. No external LLM on hot path.

kb-server (Rust) → Rig pipelines run INTERNALLY for background tasks:
                    - Queue processing (classify, extract, structure)
                    - Dedup checking (embed + compare)
                    - Auto-linking (embed + similarity threshold)
                    - Consolidation (agent with dynamic context + tool)
                    - Importance scoring (one-shot classification)
                    
Drain loop → every 5 min, processes queue + maintenance
             Uses Gemini Flash via Rig. Groq as fallback.
             If both down, queue items wait. Nothing breaks.
```

### Dependencies
```toml
rig-core = { version = "*", features = ["gemini", "groq"] }
rig-fastembed = "*"
rig-surrealdb = "*"
```
Replaces direct `fastembed` dependency with `rig-fastembed` (same ONNX engine, unified trait).

---

## 2. Taxonomy Changes

### Removals
| Subtype | Reason |
|---------|--------|
| `semantic/person` | Redundant with entity table. Memories ABOUT people are episodic/semantic with `involves` edges. |

### Moves
| Subtype | From | To | Reason |
|---------|------|----|--------|
| `signal` | `semantic` | `working` | Signals are weak/temporary. 14-day TTL. Promote to `semantic/pattern` when strengthened. |

### Additions
| Subtype | Type | TTL | Reason |
|---------|------|-----|--------|
| `outcome` | `episodic` | none | "We tried X, result was Y." Post-mortems, experiment results. Links to `prospective/experiment` via edges. |
| `assumption` | `semantic` | none | "Assuming API limit is 1000/min." Carries uncertainty. Alerts flag stale unvalidated assumptions. |
| `commitment` | `prospective` | none | "Promised Jane spec by Friday." Social accountability. Stronger than action_item. |
| `hypothesis` | `working` | 7 days | "I think bug is in auth middleware." Active investigation context. |

### Final Taxonomy (5 types, 28 subtypes)
```
episodic:     meeting, conversation, daily_log, reflection, session, research, outcome
semantic:     decision, fact, preference, value, insight, metric, pattern, assumption
procedural:   workflow, skill
prospective:  action_item, follow_up, goal, life_goal, experiment, commitment
working:      priority, open_question, scratch, signal, hypothesis
```

---

## 3. Server-Side Write Intelligence (Layer 1: No LLM)

These run synchronously on every `kb_remember` call. Pure Rust, zero latency.

### 3a. Input Schema Change
`type` and `subtype` become OPTIONAL in `RememberInput`. If omitted, server infers.

### 3b. Type/Subtype Inference (Rule-Based)
```
Keyword signals in summary → infer type/subtype
Source-based fallback (meeting source → episodic/meeting)
Default: semantic/fact (safe baseline, drain refines later)
```

### 3c. Importance Floors and Ceilings
| Subtype | Min | Max | Rationale |
|---------|-----|-----|-----------|
| decision | 4 | - | Decisions are permanent |
| value | 5 | 5 | Core beliefs never decay |
| life_goal | 5 | 5 | Life goals never decay |
| action_item | 3 | - | Tasks must surface |
| commitment | 4 | - | Social accountability |
| goal | 4 | - | Goals matter |
| scratch | - | 2 | Never pollute important results |
| daily_log | - | 2 | Context, not critical |

### 3d. Summary Quality Gate
- Reject if < 20 characters
- Reject if > 500 characters (that's content, not summary)
- Reject if no alphanumeric content

### 3e. Type/Subtype Validation
- If client provides type+subtype, validate combination
- If invalid, return error (don't silently correct)
- If only type provided, infer subtype
- If neither provided, infer both

### 3f. Auto-TTL Enforcement
```
working/scratch        → 48h (non-negotiable)
working/priority       → 7d
working/open_question  → 30d
working/signal         → 14d
working/hypothesis     → 7d
```

---

## 4. Server-Side Embedding Intelligence (Layer 2: No LLM)

These run async after write (~5-50ms). Local ONNX via rig-fastembed.

### 4a. Auto-Dedup at Write Time
```
New memory → embed summary → cosine search top-3 existing
  similarity > 0.95 → REJECT (return existing memory ID)
  similarity 0.85-0.95 → WARN (store but flag, return warning)
  similarity < 0.85 → store normally
```

### 4b. Auto-Link via Embedding Similarity
```
New memory stored → embed → cosine search top-5 existing (excluding self)
  similarity > 0.70 → auto-create `relates` edge
  similarity > 0.85 AND same project → auto-create `extends` edge
```

### 4c. Known Entity Name Matching
```
Scan summary for entity names already in DB (string match against entity name index)
Auto-create `involves` edge for matches
```

---

## 5. Retrieval Fixes

### 5a. Fix Person Filter (Entity-Anchored Query)
**Bug:** Person filter is post-hoc — drops results ranked beyond BM25 limit.  
**Fix:** When person filter present, start from entity's `involves` edges, collect memory IDs, then text-rank within that set.

### 5b. Fix Spreading Activation to Walk `involves` Edges
**Bug:** Spreading activation only walks extends/derives/relates. Ignores involves (entity links).  
**Fix:** Add `involves` edge traversal: memory→involves→entity→involves←memory. Discovers "other memories about the same person."

### 5c. Query Intent Auto-Routing
Single `kb_recall` endpoint, server auto-selects strategy:
- Person filter present → EntityAnchored (start from entity edges)
- Short keyword query → BM25-heavy weighting
- Long natural language → Vector-heavy weighting
- Temporal filters → Time-anchored scan
- Default → Balanced hybrid

No new tools. No mode parameter. Transparent to caller.

---

## 6. Decay Model Fix

### Problem
SM-2 models human forgetting, not information relevance. Health data shouldn't decay because you're busy with work. Values/decisions shouldn't decay at all.

### Fix: Domain-Aware Decay with Importance Immunity
```
NEVER DECAY (importance immune):
  importance >= 5 → immune to all decay
  subtype in [decision, value, life_goal, commitment] → immune

DOMAIN-AWARE RATES:
  health/metric memories → decay at 0.5x normal rate (infrequent access is normal)
  personal/philosophy   → decay at 0.25x rate (long-term by nature)
  working/* memories    → TTL handles expiry, no SM-2 needed
  everything else       → standard decay rate

KEEP SM-2 FOR:
  episodic memories (meetings, conversations, daily logs) → these genuinely fade
  working memories with no TTL → if somehow they exist
```

---

## 7. Dead Code and Stubs

### Remove or Wire
| Item | Action |
|------|--------|
| `assigned` edge | Wire in `kb_remember` when subtype is `action_item` or `commitment` and people field present. First person = assigned. |
| `check_stale_contacts` stub | Implement: query entities with `attrs.last_contact` older than threshold. |
| `link_person` (legacy) | Remove after migrating existing tests to use `link_entity`. |
| `find_people` (legacy) | Remove after migrating to entity-based queries. |
| `semantic/person` subtype | Remove from enum. |

---

## 8. Misclassification Feedback Loop

### Problem
No mechanism to detect or correct misclassified memories.

### Fix
- Drain session (Gemini) reviews recent memories flagged as "auto-classified"
- If Gemini disagrees with rule-based classification, update type/subtype + log the change
- New `kb_reclassify(id, new_type, new_subtype)` internal function for drain use
- Audit log: track all reclassifications for pattern detection

---

## 9. What This Does NOT Include (Out of Scope)

- Encryption at rest for sensitive memories
- Search result aggregation (SuperMemory-style)
- Multi-hop graph traversal (1-hop is sufficient for personal KB scale)
- ACT-R activation model (SM-2 with domain-aware rates is adequate for v1)
- Automatic summarization tiers (deferred to consolidation)
- Query decomposition (single queries are sufficient for personal use)
