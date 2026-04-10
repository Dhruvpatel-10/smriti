# Configuration Reference

## config.toml

```toml
[database]
path = "data/"                          # SurrealKV storage (relative to project root)

[pruning]
working_scratch_ttl_hours = 48          # scratch notes expire after 48h
working_priority_ttl_days = 7           # priorities expire after 7d
working_open_question_ttl_days = 30     # open questions expire after 30d
access_decay_interval_days = 7          # legacy decay interval for pre-SM-2 memories
consolidation_interval_hours = 24       # how often consolidation candidates are checked
importance_archive_threshold = 1        # memories at or below this importance get archived

[queue]
drain_interval_secs = 300               # drain session runs every 5 minutes
max_queue_size = 1000                   # max pending queue entries

[alerts]
overdue_action_item_days = 1            # days past event_at before action item is overdue
stale_contact_days = 14                 # days since last_contact before entity is stale
expiring_memory_hours = 24              # hours before expiry to flag for promotion

[export]
backup_dir = "archive/exports"          # where kb_export writes JSONL files
exclude_sensitive = true                # never export sensitive-visibility memories

[embeddings]
enabled = false                         # flip to true to enable vector search + dedup
model = "embeddinggemma-300m"           # ONNX model (EmbeddingGemma-300M)
dimensions = 256                        # MRL truncation (768 full, 256 recommended)

[markdown]
enabled = true                          # auto-regenerate ~/.claude/CLAUDE.md
output_path = "~/.claude/CLAUDE.md"     # supports ~ expansion
```

## Environment Variables

```
KB_DATA_DIR      — override database path
KB_QUEUE_DIR     — override queue directory
KB_ARCHIVE_DIR   — override archive directory
GEMINI_API_KEY   — Google Gemini API key (optional, for drain LLM)
GROQ_API_KEY     — Groq API key (optional, fallback LLM)
```

## MCP Server Connection

Add to `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "kb": {
      "command": "/home/stark/.cargo/bin/smriti",
      "args": [],
      "env": {
        "KB_DATA_DIR": "/home/stark/projects/smriti/data",
        "KB_QUEUE_DIR": "/home/stark/projects/smriti/queue",
        "KB_ARCHIVE_DIR": "/home/stark/projects/smriti/archive"
      }
    }
  }
}
```
