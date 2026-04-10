# Security Audit

**Date:** 2026-04-10
**Scope:** Full source audit of kb-server (personal knowledge base MCP server)
**Codebase:** /home/stark/projects/knowledge-base/src/ + config.toml + Cargo.toml
**Auditor:** Claude Opus 4.6 (1M context)

---

## Threat Model

The kb-server is a local-first personal knowledge base running as an MCP server. It stores personal memories, decisions, conversations, entity data (people, companies), and sensitive personal information in an embedded SurrealDB instance.

**Attackers:**

1. **Malicious MCP client** -- Any process that can send MCP tool calls. In stdio mode this is the directly-connected Claude session. In HTTP mode (`127.0.0.1:3001`), any local process can connect. A compromised or rogue MCP client can call any tool with any arguments.

2. **Local filesystem attacker** -- A process (or user) with read access to the machine can read the SurrealDB data directory, config files, export files, queue files, and the generated CLAUDE.md. The DB is unencrypted on disk.

3. **Network attacker (HTTP mode)** -- In HTTP transport mode the server binds to `127.0.0.1:3001` with no authentication. Any local process can issue requests. No TLS, no auth tokens, no CORS restrictions at the MCP protocol layer.

4. **Supply chain** -- Dependencies downloaded from crates.io; embedding models downloaded from HuggingFace Hub on first use.

---

## Summary

The codebase demonstrates strong security awareness in its core design: parameterized SurrealQL bindings are used consistently, enum types prevent raw string injection for type/subtype/relation fields, and visibility filtering is applied across most query paths. However, the audit identified several issues ranging from a critical arbitrary file read/write via MCP tools to medium-severity visibility bypasses and content logging violations.

**Findings by severity:**
- Critical: 2
- High: 3
- Medium: 7
- Low: 5
- Info: 4

---

## Findings

### [CRITICAL] filesystem -- tools/export.rs:33-36 -- Arbitrary file write via kb_export path parameter

The `kb_export` tool accepts an optional `path` parameter that is used directly as the export directory with no validation or sandboxing:

```rust
let dir = input
    .path
    .map(PathBuf::from)
    .unwrap_or_else(|| export_dir.to_path_buf());
```

A malicious MCP client can call `kb_export(path="/etc/cron.d")` or `kb_export(path="/home/stark/.ssh")` to write JSONL files to arbitrary directories. While the filename is auto-generated (`kb_export_TIMESTAMP.jsonl`), the directory is fully attacker-controlled. Combined with the fact that exported data contains memory content, this is also a data exfiltration path -- an attacker can export to a world-readable directory.

**Impact:** Arbitrary file creation in any directory writable by the server process. Data exfiltration of all non-sensitive memories.

**Recommendation:** Validate that the export path is within the configured `export_dir` or reject absolute paths. Alternatively, remove the `path` parameter entirely and always use the configured directory.


### [CRITICAL] filesystem -- tools/export.rs:58-63 -- Arbitrary file read via kb_import path parameter

The `kb_import` tool accepts a `path` parameter with no validation:

```rust
let path = std::path::Path::new(&input.path);
let result = export::import_from(db, path).await?;
```

A malicious MCP client can read any file on the filesystem by providing its path. While the file must be valid JSONL, the error messages will reveal file existence and partial content. More critically, a crafted JSONL file placed elsewhere on the system could be used to inject arbitrary data into the database, potentially overwriting existing memories or entities (the import uses `INSERT INTO memory CONTENT $data` which could conflict with existing records).

**Impact:** Arbitrary file read (any JSONL-shaped file). Database poisoning via crafted import files at known paths.

**Recommendation:** Restrict import paths to within the configured export directory. Validate that imported files were produced by this system (e.g., check for a header line with a version/signature).


### [HIGH] visibility-bypass -- db/entity.rs:261-301 -- Entity recent_memories leaks private/sensitive memories

The `recent_memories()` function (called by `kb_entity_get`) fetches memories linked to an entity with only `status = 'active'` filtering:

```sql
SELECT * FROM memory
WHERE id IN (SELECT in FROM involves WHERE out = $entity).in
  AND status = 'active'
ORDER BY event_at DESC
LIMIT $n
```

There is **no visibility filter**. A malicious client can call `kb_entity_get` on any entity to see summaries of private and sensitive memories associated with that entity. This completely bypasses the visibility system.

**Impact:** Full summary content of private and sensitive memories leaked through entity relationship traversal.

**Recommendation:** Add `AND visibility = 'standard'` (or appropriate project-aware visibility filter) to the query in `recent_memories()`.


### [HIGH] visibility-bypass -- db/alerts.rs:26-67 -- Alerts leak sensitive memory summaries

The `check_overdue_actions` and `check_expiring_memories` functions query memories without any visibility filter:

```sql
SELECT * FROM memory
WHERE status = 'active'
  AND memory_type = 'prospective'
  AND subtype = 'action_item'
  AND event_at < $cutoff
```

The alert message then includes the full summary text: `format!("Action item overdue: {summary}")`. A malicious client calling `kb_alerts` will see summaries of sensitive action items and expiring sensitive memories.

**Impact:** Sensitive memory summaries exposed in alert messages.

**Recommendation:** Add `AND visibility != 'sensitive'` to both alert queries. Consider adding `AND visibility != 'private'` as well since alerts are cross-project.


### [HIGH] visibility-bypass -- db/alerts.rs:167-216 -- Contradiction alerts leak sensitive memory summaries

The `check_unsuperseded_updates` function queries the `updates` edge table and reads `in.summary` and `out.summary` with no visibility filter:

```sql
SELECT
    record::id(in) AS new_id,
    record::id(out) AS old_id,
    in.summary AS new_summary,
    out.summary AS old_summary
FROM updates
WHERE out.status = 'active'
LIMIT 10
```

Both old and new memory summaries from sensitive memories appear in the contradiction alert message.

**Impact:** Sensitive memory summaries exposed through contradiction detection alerts.

**Recommendation:** Add visibility filtering: `WHERE out.status = 'active' AND out.visibility != 'sensitive' AND in.visibility != 'sensitive'`.


### [MEDIUM] visibility-bypass -- db/search.rs:209-216 -- Spreading activation excludes sensitive but leaks private memories cross-project

The spreading activation neighbor fetch correctly excludes sensitive:

```sql
AND visibility != 'sensitive'
```

However, it does not check whether the query is project-scoped or cross-project. In cross-project recall (no project filter), private memories should be excluded (`visibility = 'standard'` only), but spreading activation always allows private memories through. A cross-project recall query could surface private memories from unrelated projects via graph neighbor traversal.

**Impact:** Private memories from unrelated projects can leak through graph-edge expansion during cross-project recall.

**Recommendation:** Pass the project context into `spreading_activation()` and apply the same visibility logic as the main query: if project is None, filter to `visibility = 'standard'`; if project is Some, filter `visibility != 'sensitive'`.


### [MEDIUM] visibility-bypass -- db/consolidation.rs:38-55 -- Consolidation candidates leak sensitive memory summaries

The consolidation candidate detection queries filter `visibility != 'sensitive'` which is correct, but the results include full `summaries` arrays that are returned to the MCP client. However, the **graph cluster heuristic** (lines 80-92) also uses `visibility != 'sensitive'` but does not filter private memories in cross-project context. Since consolidation candidates are always returned globally, private memory summaries from any project can appear.

**Impact:** Private memory summaries exposed in consolidation candidate results across projects.

**Recommendation:** Either filter `visibility = 'standard'` for both heuristics, or add a project filter option to the consolidation candidates tool.


### [MEDIUM] visibility-bypass -- db/search.rs:662-696 -- Status query excludes sensitive but allows private cross-project

The `status_query` function uses `visibility != 'sensitive'` for action items, questions, and decisions. However, when called without a project filter (cross-project status), private memories from any project are included. The standard recall path correctly uses `visibility = 'standard'` for cross-project queries.

**Impact:** Private memories appear in cross-project status reports.

**Recommendation:** Apply the same visibility logic as recall: `visibility = 'standard'` when project is None, `visibility != 'sensitive'` when project is Some.


### [MEDIUM] privacy-logging -- db/alerts.rs:53-63 -- Summary content in alert messages exposed to MCP client

While not a logging issue per se, the alert system constructs messages containing full memory summaries: `"Action item overdue: {summary}"`, `"Memory expiring soon: {summary}"`, and `"superseded by \"{new_summary}\""`. These messages are returned to any MCP client via `kb_alerts`. Combined with the lack of visibility filtering (see HIGH findings above), this creates a content exposure channel.

**Impact:** Memory summary content returned in alert messages to any connected MCP client.

**Recommendation:** Either filter sensitive content from alerts, or return only memory IDs in alerts and require `kb_drill` for content access (which at least provides an audit trail via access_count).


### [MEDIUM] denial-of-service -- tools/remember.rs -- No size limit on content field

The `content` field in `kb_remember` has no size validation. While `summary` is limited to 500 chars, `content` can be arbitrarily large. A malicious client could store multi-gigabyte content blobs, exhausting disk space in the SurrealDB data directory.

The chunking system (`CHUNK_THRESHOLD = 2000`) will also create many sub-memories for very large content, amplifying storage impact.

**Impact:** Disk exhaustion via unbounded content storage. Memory exhaustion if extremely large content triggers chunking with many spawned embed tasks.

**Recommendation:** Add a content size limit in validation (e.g., 100KB or 1MB). Also limit the number of chunks per parent memory.


### [MEDIUM] denial-of-service -- db/auto_link.rs:95-130 -- Entity name scan is O(entities * summary_length)

The `match_known_entities` function loads ALL entity names from the database, then does substring matching against the summary. With many entities (thousands), this becomes expensive and is called on every `kb_remember` in the post-create async path. A malicious client could create thousands of entities via `kb_entity_upsert`, then each new memory triggers a full scan.

**Impact:** CPU exhaustion via entity name matching with many entities.

**Recommendation:** Add a limit to the entity name query (e.g., LIMIT 1000) or use a database-side CONTAINS query instead of client-side iteration.


### [MEDIUM] data-integrity -- tools/remember.rs:164-179 -- Supersession does not verify old memory is active before superseding

Looking more carefully at the code, there IS a check:

```rust
Ok(old_mem) if old_mem.status == MemoryStatus::Active => {
    memory::supersede(db, &old_id).await?;
```

This correctly checks that the target is active. However, the `memory::supersede` function itself has no such guard -- it will set status to 'superseded' regardless of current status:

```sql
UPDATE type::record('memory', $id) SET status = 'superseded', updated_at = time::now()
```

If two concurrent `kb_remember` calls both target the same `updates_id`, both will see `status == Active` and both will supersede it. The race window is small but real. More importantly, `supersede()` can be called on already-archived memories if the check and the update are not atomic.

**Impact:** Race condition could leave the system in an inconsistent state with duplicate supersession.

**Recommendation:** Make the supersede operation atomic by adding `WHERE status = 'active'` to the UPDATE query and checking the return count.


### [MEDIUM] injection -- db/graph.rs:32-35 -- Table name interpolated into SurrealQL from enum

The `link_memories` function interpolates `table` (from `RelationType::to_string()`) directly into SQL:

```rust
let sql = format!(
    "LET $existing = (SELECT * FROM {table} WHERE in = $from AND out = $to);
     IF $existing = [] {{ RELATE $from -> {table} -> $to }};"
);
```

While `table` derives from the `RelationType` enum (which is safe), this pattern is fragile. If a new `RelationType` variant were added with a name that collides with a SurrealQL keyword or contains special characters, it would create an injection. The comment in the code acknowledges this: "Since `table` derives from a safe enum (not user input), direct interpolation is acceptable."

**Impact:** Currently safe. Would become a vulnerability if enum variants are added carelessly.

**Recommendation:** Add a compile-time or startup assertion that all `RelationType` display values are valid SurrealDB identifiers. Consider using a whitelist match instead of `to_string()`.


### [LOW] authentication -- main.rs:162 -- HTTP transport has no authentication

In HTTP mode, the server binds to `127.0.0.1:3001` with no authentication mechanism. Any local process can connect and call any tool. While binding to localhost prevents remote access, any malware or compromised process on the machine has full access to the knowledge base.

**Impact:** Any local process can read, write, delete, and export all memories.

**Recommendation:** Add a bearer token authentication mechanism for HTTP mode. Generate a random token at startup and require it in the Authorization header. Store the token in a file readable only by the user (mode 0600).


### [LOW] filesystem -- config.rs:153-157 -- Queue directory from environment variable without validation

The `queue_dir()` reads from `KB_QUEUE_DIR` environment variable without path validation:

```rust
pub fn queue_dir(&self) -> PathBuf {
    std::env::var("KB_QUEUE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("queue"))
}
```

An attacker who can set environment variables could redirect queue writes to arbitrary directories.

**Impact:** Queue file writes to attacker-controlled directories. Low severity because environment variable access typically implies broader system compromise.

**Recommendation:** Validate that queue_dir is within the project directory or an expected location.


### [LOW] filesystem -- db/markdown.rs:129-148 -- Markdown output path from config writes to arbitrary location

The markdown `output_path` in config supports `~` expansion and writes to any path:

```rust
pub fn resolved_path(&self) -> std::path::PathBuf {
    let path = &self.output_path;
    if let Some(rest) = path.strip_prefix("~/") { ... }
    std::path::PathBuf::from(path)
}
```

A malicious config could set `output_path = "/etc/profile.d/evil.sh"` to write markdown content to a sensitive location. Since the markdown content includes memory summaries (filtered to `visibility = 'standard'`), this is also a data exposure path.

**Impact:** Arbitrary file write of markdown content to any location writable by the process. Requires config file modification.

**Recommendation:** Validate that the output path is within expected locations (home directory or project directory).


### [LOW] privacy-logging -- db/alerts.rs:53-56 -- Alert messages contain memory summary text

Alert messages include verbatim summary text:

```rust
format!("Action item overdue: {summary}")
format!("Memory expiring soon: {summary}")
```

If tracing is configured at info level (the default), `tracing::info!(count = alerts.len(), "alert check complete")` does not log content. However, the messages are returned to MCP clients, which may log them. This is a privacy concern for sensitive data.

**Impact:** Summary text in structured data returned to clients (not in server logs directly).

**Recommendation:** Consider using IDs instead of summaries in alert messages, or ensure visibility filtering prevents sensitive content from reaching alerts (see HIGH findings).


### [LOW] denial-of-service -- tools/queue.rs:30-33 -- Queue size check is TOCTOU race

The queue size check reads the file, counts lines, then appends. Two concurrent calls could both pass the size check and both write, exceeding max_queue_size:

```rust
let line_count = content.lines().filter(|l| !l.trim().is_empty()).count();
if line_count >= max_size {
    return Err(KbError::Queue(...));
}
```

**Impact:** Queue size limit can be bypassed via concurrent calls. Low severity because the queue is a bounded append-only file.

**Recommendation:** Use file locking (e.g., `flock`) or an atomic size check.


### [INFO] injection -- All SurrealQL queries use parameterized bindings

All user-controlled values (summary, content, project, domain, people, tags, entity names, query strings) are passed through `.bind()` parameters. No string interpolation of user input was found in any SurrealQL query. The only `format!` interpolations in SQL strings use:

1. Static WHERE clause fragments built from Option checks (search.rs)
2. Enum-derived table names (graph.rs -- see MEDIUM finding)
3. Static immune subtype lists (prune.rs)

**Impact:** No SurrealQL injection vectors were found.


### [INFO] data-integrity -- kb_forget only soft-deletes

The `memory::archive()` function correctly sets `status = 'archived'` without any DELETE operation. No hard-delete function exists in the codebase. The `archive()` function uses `RETURN BEFORE` to verify the record existed.

**Impact:** Correct behavior as designed.


### [INFO] supply-chain -- fastembed model downloads from HuggingFace Hub

The `fastembed` crate downloads ONNX models from the HuggingFace Hub on first use. The download happens over HTTPS and models are cached locally. There is no integrity verification (hash checking) of downloaded models beyond what fastembed provides internally.

If the HuggingFace Hub were compromised or a MITM attack on the HTTPS connection succeeded, a malicious model could be served. The impact would be degraded embedding quality (not code execution, since ONNX Runtime is a sandboxed inference engine).

**Impact:** Model integrity depends on HuggingFace Hub HTTPS security and fastembed's download mechanism.

**Recommendation:** Pin model versions and verify checksums if fastembed supports it.


### [INFO] supply-chain -- LLM API keys handled correctly

LLM API keys (GEMINI_API_KEY, GROQ_API_KEY) are read from environment variables only (never from config files) and are never logged. The `tracing::info!("Gemini LLM client initialized")` log line does not include the key. The Rig framework handles the keys internally.

**Impact:** API key handling follows best practices.


### [INFO] data-integrity -- Importance Importance::new() enforces 1-5 range at Rust type level

The `Importance` newtype validates the 1-5 range at construction time. However, the SurrealDB schema defines `importance` as `TYPE int` with no CHECK constraint. A direct database manipulation (or import of a crafted JSONL file) could set importance outside the 1-5 range. The prune system would then use the out-of-range value in its decay logic, potentially causing unexpected behavior (e.g., importance of 100 would never decay to archive threshold).

**Impact:** Data integrity bypass requires direct DB access or malicious import file.

**Recommendation:** Add a CHECK constraint in the SurrealDB schema: `DEFINE FIELD importance ON TABLE memory TYPE int ASSERT $value >= 1 AND $value <= 5`.

---

## Recommendations

### Priority 1 (Fix immediately)

1. **Sandbox export/import paths** -- Validate that `kb_export.path` resolves within the configured export directory. Validate that `kb_import.path` resolves within the configured export directory. Reject absolute paths or paths containing `..`.

2. **Add visibility filtering to entity recent_memories** -- Add `AND visibility = 'standard'` to the query in `db/entity.rs:recent_memories()`.

3. **Add visibility filtering to all alert queries** -- Add `AND visibility != 'sensitive'` to `check_overdue_actions`, `check_expiring_memories`, and `check_unsuperseded_updates`.

### Priority 2 (Fix soon)

4. **Fix spreading activation visibility for cross-project queries** -- Pass project context and apply matching visibility filter.

5. **Fix status_query visibility for cross-project queries** -- Use `visibility = 'standard'` when project is None.

6. **Add content size limit** -- Validate content length in `remember.rs` (suggest 1MB max).

7. **Add authentication for HTTP transport** -- Generate a random bearer token on startup.

### Priority 3 (Harden)

8. **Add SurrealDB CHECK constraints** -- For importance range and status/visibility enum values.

9. **Make supersede atomic** -- Add `WHERE status = 'active'` to the UPDATE in `memory::supersede()`.

10. **Cap entity name scan** -- Add LIMIT to entity query in `match_known_entities()`.

11. **Add file locking to queue writes** -- Prevent TOCTOU race on queue size check.

12. **Validate markdown output_path** -- Restrict to home directory or project directory.

### Defense in Depth

13. **Encrypt SurrealDB at rest** -- The data directory contains all memories in plaintext. Consider SurrealKV encryption if available, or full-disk encryption.

14. **Audit log** -- Log all tool invocations (tool name + memory IDs only, never content) to a separate audit file for forensic analysis.

15. **Rate limiting** -- In HTTP mode, add per-session rate limiting to prevent DoS from a compromised local process.
