# Smriti

Persistent second memory for your personal AI system.

A local-first MCP server backed by embedded SurrealDB — everything you've learned, decided,
and experienced, recallable across any Claude Code session, project, or device.
No cloud. No external database. No configuration required to get started.

---

## What it does

Smriti gives Claude Code a persistent memory layer that survives across sessions:

- **Remember** decisions, conversations, goals, and facts with a structured taxonomy
- **Recall** by natural language query — hybrid BM25 + vector search with RRF fusion
- **Decay** unimportant memories over time via SM-2 spaced repetition
- **Link** memories into a graph (updates, extends, derives, relates)
- **Deduplicate** writes automatically using cosine similarity before storing
- **Spread** activation across related memories during retrieval

---

## Requirements

- Rust 1.85+ (edition 2024)
- No external database — SurrealDB runs embedded

---

## Quick start

```bash
# Build and install the binary
cargo install --path .

# Create data directories
mkdir -p data queue archive

# Wire into Claude Code (~/.claude/settings.json)
scripts/install.sh
```

Add to `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "kb": {
      "command": "/home/you/.cargo/bin/smriti",
      "args": [],
      "env": {
        "KB_DATA_DIR": "/path/to/smriti",
        "KB_CONFIG_DIR": "/path/to/smriti"
      }
    }
  }
}
```

---

## Memory taxonomy

Five types, 23 subtypes — caller classifies, server validates.

```
episodic (6):    meeting, conversation, daily_log, reflection, session, research
semantic (6):    decision, fact, preference, value, metric, pattern
procedural (2):  workflow, skill
prospective (4): action_item, goal, life_goal, experiment
working (5):     priority (7d), open_question (30d), scratch (48h), signal (14d), hypothesis (7d)
```

Working memories have enforced TTLs and expire automatically.
High-importance memories (`importance >= 5`) and protected subtypes (`decision`, `value`, `life_goal`) are immune to decay.

---

## MCP tools

| Tool | Purpose |
|------|---------|
| `kb_remember` | Store a memory (validate → embed → dedup → create) |
| `kb_recall` | Hybrid search: BM25 + vector + spreading activation |
| `kb_recall_at` | Point-in-time recall as of a past date |
| `kb_drill` | Full memory details + provenance graph |
| `kb_forget` | Soft-archive (never hard-delete) |
| `kb_promote` | Clear TTL, bump importance |
| `kb_link` | Create typed graph edges between memories |
| `kb_consolidate` | Merge a set of memories into one |
| `kb_queue` | Queue an instruction for async processing |
| `kb_status` | Summary: active counts, overdue items, top entities |
| `kb_alerts` | Overdue actions, stale contacts, expiring memories |
| `kb_entity_upsert` | Create or update a person/company/place entity |
| `kb_entity_find` | Search entities |
| `kb_entity_get` | Full entity record + recent memories |
| `kb_people` | Find person entities by name |
| `kb_export` | Export all memories to JSONL |
| `kb_import` | Import from JSONL backup |
| `kb_regenerate_md` | Regenerate the Layer 1 markdown file |
| `kb_reembed` | Re-embed all memories (after model/dim change) |

---

## Configuration

`config.toml` at the project root:

```toml
[database]
path = "data/"

[embeddings]
enabled = false          # flip to true for vector search + dedup
model = "embeddinggemma-300m"
dimensions = 256

[pruning]
importance_archive_threshold = 1
access_decay_interval_days = 7
consolidation_interval_hours = 24

[export]
backup_dir = "archive/exports"
exclude_sensitive = true
```

Full reference: [`docs/config.md`](docs/config.md)

---

## Architecture

```
Write path:   validate → embed → dedup → create → (async) store vector, link entities, auto-link, chunk
Retrieval:    BM25 → vector cosine → RRF fusion → entity-anchored filter → spreading activation
Decay:        SM-2 spaced repetition — overdue + unaccessed memories lose importance over time
Storage:      SurrealDB (kv-surrealkv embedded) — single-file, no daemon, git-friendly path
Transport:    stdio (default, one process per session) or HTTP (daemon, shared across sessions)
```

Detailed docs in [`docs/`](docs/).

---

## Running as a daemon (HTTP transport)

```bash
KB_TRANSPORT=http KB_HTTP_PORT=3001 smriti

# Or with systemd
cp scripts/smriti.service ~/.config/systemd/user/
systemctl --user enable --now smriti
```

In daemon mode, all Claude Code sessions share one process and one database connection.

---

## License

Licensed under the [GNU Affero General Public License v3.0](LICENSE).

You are free to use, modify, and distribute this software. If you modify it and
run it as a service or distribute it, you must release your changes under the
same license. Commercial use without open-sourcing modifications is not permitted.
