# Logging & Observability Audit

## Current Setup

**Subscriber:** `tracing_subscriber::fmt()` — plain-text, human-readable format (not JSON).

**Default log level:** `INFO`. Controlled via `RUST_LOG` env var; falls back to `EnvFilter::new("info")` if unset.

**Output:** All logging goes to `stderr` via `.with_writer(std::io::stderr)`. Correct — stdout is reserved for the MCP protocol.

**No log rotation or file sink.** Logs go to stderr only. No file output, no rotation, no size limit. For a long-running HTTP daemon this means logs either disappear into the terminal or require the shell/systemd to handle redirection.

**No structured (JSON) logging.** The `fmt()` subscriber emits human-readable plain text. Fields passed as named arguments (`memory_type = %memory_type`) are included, but the output is not machine-parseable.

**`#[instrument]` coverage:** Widespread across all DB-layer functions. Most public async functions in `src/db/` carry `#[instrument]`. The tool-layer execute functions do NOT carry `#[instrument]` (see "Missing Instrumentation" below).

---

## Privacy Violations

### `src/db/alerts.rs:60` — memory summary in alert message
```rust
alerts.push(Alert {
    message: format!("Action item overdue: {summary}"),
```
`summary` is the full text of a memory. This is set as the alert's `.message` field which is returned to callers (and ultimately displayed). The alert itself is not logged directly here, but the summary flows into `check_all()` which logs `count = alerts.len()` at INFO — so the count alone is fine. **However**, the message strings containing raw summaries end up in the `Alert` struct that is serialized and returned. This is borderline — the summaries are not emitted to the log stream, but alert messages are included in `kb_regenerate_md()` output that is written to `~/.claude/CLAUDE.md`.

### `src/db/alerts.rs:104` — same issue for expiring memory summary
```rust
message: format!("Memory expiring soon: {summary}"),
```
Same as above — raw summary embedded in alert message.

### `src/db/alerts.rs:149–155` — entity name in stale contact alert (logged indirectly)
```rust
message: format!("No contact with {name} since {}",
```
`name` is a real person's name from the entity table. Not written to the log stream directly, but embedded in `Alert.message`.

### `src/db/alerts.rs:207–211` — both old and new summaries in contradiction alert
```rust
message: format!(
    "Memory still active despite being updated: \"{old_summary}\" superseded by \"{new_summary}\""
),
```
Two raw memory summaries concatenated into the alert message. Same boundary: not logged directly but propagated to callers and potentially to the CLAUDE.md file.

### `src/db/auto_link.rs:109` — entity name in warning
```rust
Err(e) => warn!(entity = name, "entity auto-link failed: {e}"),
```
`name` is an entity name (real person's name matched from summary text). This IS written to the tracing stream at WARN level. **Privacy violation** — entity names are PII.

### `src/db/entity.rs:20` — `#[instrument]` with `fields(name = %name)`
```rust
#[instrument(skip(db, attrs, context), fields(name = %name, kind = %kind))]
pub async fn upsert(...)
```
The `name` field is a person's name. It will appear in every span/log emitted from within `upsert()` and in parent spans. **Privacy violation** — entity names are PII.

### `src/db/entity.rs:133` — entity name in error string
```rust
.map_err(|e| KbError::db(format!("getting entity by name '{name}'"), e))?;
```
The `KbError::db` message includes `name` (person's name). If this error is ever logged upstream, the name leaks. The error is propagated to tool handlers which format it as `{"error": "..."}` — not written to tracing directly, but error context includes the name.

### `src/db/entity.rs:133, 56, 84` — multiple error contexts with entity names
Several `KbError::db(format!("updating entity {}", entity.id), e)` calls include entity IDs (which are opaque UUIDs, fine) but also `format!("getting entity by name '{name}'"` which includes the raw name. If errors propagate into tracing via upstream `?` handling, names leak.

### `src/main.rs:56` — export file path in INFO log
```rust
info!("backup complete: {}", result.path);
```
`result.path` is an absolute filesystem path to the export file. For a personal system this is low-risk, but it reveals the KB data directory layout to anyone reading logs.

### `src/db/export.rs:83–88` — export file path in INFO log
```rust
tracing::info!(
    path = %file_path.display(),
    ...
    "export complete"
);
```
Same as above — file path in log output.

### `src/db/export.rs:103` — import file path in span field
```rust
#[instrument(skip(db), fields(path = %path.display()))]
pub async fn import_from(...)
```
The import path (which is constructed from user-supplied filename + configured export dir) appears in the span. Acceptable for personal use but worth noting.

### `src/db/consolidation.rs:71–73` — summaries included in `ConsolidationCandidate`
The `ConsolidationCandidate` struct contains `summaries: Vec<String>` (raw memory summaries). This struct is logged in `main.rs` only as `count = candidates.len()` — the summaries are not logged — so this is **not a current violation**, but the data structure is dangerous if a future log call dumps the candidates.

### `src/llm/classify.rs:54` — summary passed to LLM (not logged, but worth noting)
```rust
let user_prompt = format!("Classify this memory:\n\n{summary}");
```
The summary is sent to an external LLM provider (Gemini/Groq). This is outside the logging scope but is the most significant privacy concern in the codebase. No logging of summary content here, which is correct.

---

**Summary of confirmed tracing-stream violations:**
- `src/db/auto_link.rs:109` — entity name (real person name) at WARN
- `src/db/entity.rs:20` — entity name in `#[instrument]` span field at INFO/DEBUG

---

## Missing Observability

### Startup logging (src/main.rs)
- **No startup banner.** The server logs "starting kb-server (stdio transport)" but does NOT log:
  - Which config file was loaded and from which directory
  - Which DB path is being used (only the path appears in the `connect` span at DEBUG, not surfaced at INFO)
  - The transport mode chosen (`stdio` vs `http`)
  - The embedding model name and dimensions at startup (only logged in `KbServer::new`, which is correct, but the DB path / config path are never logged)
  - The server version (no `env!("CARGO_PKG_VERSION")` log)

### Memory count at startup
No call to `memory::count()` on startup. A DBA wants to know "started with 1,247 active memories" to detect data loss between restarts. Currently zero visibility into DB state at boot.

### Tool call tracing
The tool handler functions in `src/tools/` (`remember::execute`, `recall::execute`, etc.) have no `#[instrument]` attributes and no per-call logging. There is no way to see:
- Which tool was called
- How long it took
- Whether it succeeded or failed
- The memory type/subtype for `kb_remember` calls (without diving into DB spans)

The only per-call logging that exists is in the DB layer (via `#[instrument]`), but those spans don't surface at INFO level and aren't tied to "tool X was called and took Y ms".

### Dedup rejections
`src/tools/remember.rs:99–109` handles dedup results silently. A duplicate rejection returns `Err(...)` which propagates to the tool handler and becomes `{"error": "..."}` — there is no INFO or WARN log that says "dedup rejected write (similarity 0.94, matched memory:abc123)". You cannot tell from logs that dedup is functioning or how often it fires.

### Dedup warnings (near-duplicates)
`src/tools/remember.rs:104–106` sets `warning_msg` but never logs it. The warning appears in the response message only, not in the log stream.

### Supersession events
`src/tools/remember.rs:171–173` handles successful supersessions silently. When `supersede()` returns `Ok(true)`, nothing is logged. The WARN for failed supersession exists, but not the INFO for successful ones. You cannot audit "memory X was superseded by memory Y" from logs.

### Pruning results detail
`src/db/prune.rs:54` logs `info!(expired, decayed, archived, "pruning complete")` which is good, but:
- Does not log which pruning strategy was skipped (e.g., if SM-2 ran but legacy did not apply)
- Does not log timing (how long did pruning take on startup?)
- The prune failure log in `main.rs:62` says "initial pruning failed" but does not include the error detail inline (the `{e}` is included — this is fine actually)

### Auto-link results
`src/db/auto_link.rs:150,166` logs entity auto-link and similarity auto-link counts at INFO only when count > 0. This is reasonable, but:
- There is no log when auto-link runs with 0 results (normal, but invisible)
- There is no aggregate log at any level of total auto-link edges created per session

### Embedding model load
`src/db/embeddings.rs:58–66` logs "loading embedding model" at INFO on first use, then "embedding model loaded" after success. This is reasonable but:
- Does not log the model name that was loaded (only logged in `server.rs:30` at construction, which precedes the actual load)
- Does not log how long the load took (no timing around `TextEmbedding::try_new`)

### HTTP transport: no request logging
`src/main.rs:178` logs `tracing::debug!(%addr, "new connection")` but there is no per-request logging for HTTP mode (no path, no method, no response status, no latency). Debug-only connection logging is insufficient for daemon mode.

### Config validation errors
`src/config.rs` has no logging at all. Config load failures are propagated as errors (good), but a successful config load emits nothing — no "loaded config from /path/config.toml" message.

### Migration outcomes
`src/db/migrations.rs:161` logs "schema up to date" at INFO and per-migration "applying migration" / "migration applied" at INFO. This is good. No gaps here.

### Queue operations
`src/tools/queue.rs` has no logging. Queue writes and size-limit rejections are invisible in the log stream.

### Reembed progress
`src/db/reembed.rs:100` logs batch progress at INFO. This is good.

---

## Log Level Issues

### `src/db/embeddings.rs:68` — ERROR for a recoverable state
```rust
tracing::error!("{msg}");
```
Embedding model load failure is logged at ERROR. However, the server continues running (embeddings degrade gracefully — writes proceed without vectors). This is WARN territory since functionality is degraded but the server is not unrecoverable. Using ERROR here will alarm monitoring tools unnecessarily.

### `src/db/auto_link.rs:64,109` — WARN for expected conditions
```rust
Err(e) => warn!(from = %memory_id, to = %similar_id, "auto-link failed: {e}"),
Err(e) => warn!(entity = name, "entity auto-link failed: {e}"),
```
Auto-link failures are expected in concurrent scenarios (memory deleted between similarity search and link creation). WARN implies degraded functionality worth investigating. These are better at DEBUG for routine idempotency failures.

### `src/db/search.rs:236,758` — WARN for malformed rows
```rust
tracing::warn!("skipping malformed neighbor row: {e}");
tracing::warn!("skipping malformed memory row: {e}");
```
Malformed rows indicate a data integrity issue that warrants WARN — this is correct. However, these could spam WARN during a migration if old-format rows exist. Consider DEBUG after a migration window.

### `src/db/search.rs:557` — WARN for spreading activation failure
```rust
tracing::warn!("spreading activation failed: {e}");
```
Spreading activation is a best-effort enhancement. Failure here doesn't degrade core functionality — results are just less connected. This is DEBUG-level.

### `src/llm/client.rs:92` — WARN for Gemini primary failure with Groq fallback available
```rust
tracing::warn!("Gemini prompt failed: {e}, trying Groq fallback");
```
This fires every time Gemini is rate-limited or has a transient error. When Groq fallback is available and succeeds, the overall operation succeeds — this should be DEBUG, not WARN.

### `src/main.rs:127` — INFO for stdio transport start is misleading
```rust
info!("starting kb-server (stdio transport)");
```
In stdio mode, the server starts and immediately blocks waiting for MCP messages. No further startup state is logged (no DB path, no memory count). The log gives false confidence that "it started" when critical post-start operations (pruning, etc.) run silently.

---

## Structured Logging Gaps

### `#[instrument]` skipping useful fields

**`src/db/memory.rs:12`** — `summary` is skipped (correct for privacy) but `source`, `project`, and `people` are also skipped even though they are safe metadata:
```rust
#[instrument(skip(db, summary, content), fields(memory_type = %memory_type, subtype = %subtype))]
```
`project` and `source` would be useful debug fields and contain no PII.

**`src/db/search.rs:264`** — `query` text is skipped (correct for privacy) and `query_vector` is skipped (correct — huge). But `memory_type`, `project`, `domain`, and `limit` are not captured as span fields:
```rust
#[instrument(skip(db, query, query_vector))]
```
These are all safe and would help debug "why did this search return 0 results?"

**`src/db/prune.rs:49`** — `skip_all` means the entire pruning invocation has no fields in the span. Adding `fields(run_at = ...)` would help correlate pruning logs to wall-clock time.

### Missing `#[instrument]` on public functions

The following public functions lack `#[instrument]`:
- `src/tools/remember::execute` — the top-level tool entry point
- `src/tools/recall::execute` — same
- `src/tools/recall_at::execute` — same
- `src/tools/drill::execute` — same
- `src/tools/forget::execute` — same
- `src/tools/promote::execute` — same
- `src/tools/consolidate::execute` — same
- `src/tools/entity::execute_upsert`, `execute_find`, `execute_get` — same
- `src/tools/export::execute_export`, `execute_import` — same
- `src/tools/queue::execute` — same
- `src/db/memory::count` — utility, but used nowhere with logging context
- `src/db/prune::domain_decay_rate` — trivial, skip
- `src/llm/client::LlmClient::from_env`, `prompt` — LLM calls have no span

Without spans on tool execute functions, it is impossible to correlate a DB error back to which tool invoked it — the spans jump from MCP handler directly into DB calls.

### `src/db/entity.rs:20` — `name` field in span (PII)
```rust
#[instrument(skip(db, attrs, context), fields(name = %name, kind = %kind))]
```
`name` is a person's real name. It should be skipped. Only `kind` and the resulting entity ID should appear in spans.

### Span names not explicitly set
`#[instrument]` without `name = "..."` uses the function name as the span name, which is fine for most cases. But tool-layer functions would benefit from explicit names like `name = "kb_remember"` to match the MCP tool names users invoke.

---

## Recommended Logging Strategy

### Format: Plain text for interactive use, JSON optional for daemon mode

For a personal server run interactively (stdio mode, terminal), plain-text `tracing_subscriber::fmt()` is the right default — it's readable without tooling. For daemon HTTP mode, add an opt-in JSON format:

```rust
let use_json = std::env::var("KB_LOG_JSON").is_ok();
if use_json {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
} else {
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
```

### Output: stderr only (current behavior is correct)

Keep stderr-only. For daemon mode, redirect stderr to a file at the process/systemd level:
```
KB_TRANSPORT=http ./kb-server 2>> ~/.local/share/kb-server/kb-server.log
```

### Log rotation: Use an external tool, not in-process

For a personal server, `logrotate` or systemd journal is simpler than in-process rotation. If you want in-process rotation, add `tracing-appender` as a dependency:
```toml
tracing-appender = "0.2"
```
```rust
let file_appender = tracing_appender::rolling::daily(
    std::env::var("KB_LOG_DIR").unwrap_or_else(|_| "~/.local/share/kb-server/logs".into()),
    "kb-server.log",
);
let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
// use non_blocking as the writer
```
Keep the `_guard` alive for the process lifetime (store in main). For stdio mode, keeping stderr is fine — processes exit quickly.

### What each level should contain

**ERROR** — unrecoverable failures only: DB connection failed and cannot continue, config file missing/invalid (process must exit), embedding model load failure (currently logged at ERROR but should be WARN since the server continues).

**WARN** — degraded functionality that affects correctness or quality but does not crash:
- Embedding model load failed (server continues without vectors)
- Dedup check failed (storing anyway)
- Vector search failed (falling back to BM25 only)
- LLM provider down (classification/drain disabled)
- Queue full (write rejected)
- Supersession target not found (edge not created, data may be inconsistent)

**INFO** — operational events a server admin looks at once per run:
- Server started (with transport, listen address, DB path, server version)
- Memory count at startup (active/total)
- Embedding model name and dimensions (at construction, currently correct)
- Migration outcomes (currently correct)
- Pruning results: expired/decayed/archived counts with timing
- Export/import complete with counts and path
- Auto-CLAUDE.md regenerated
- Consolidation candidates found (currently correct)
- LLM providers available/unavailable (currently correct)

**DEBUG** — per-request details useful when actively debugging a problem (`RUST_LOG=debug`):
- Each tool call: tool name, key params (type/subtype/project, NOT content/summary), duration
- Dedup result: clean/warning/duplicate with similarity score and matched ID
- Supersession events (old_id superseded by new_id)
- Auto-link results (count of edges created per memory, even when 0)
- Spreading activation results
- Vector search timing
- Query result counts

**TRACE** — verbose internals, normally off (`RUST_LOG=trace`):
- Individual SQL queries and their timing
- SM-2 computation details
- Chunk creation details
- Per-row parse results

### How to enable debug mode for troubleshooting

Set `RUST_LOG` before launching:
```bash
# All debug output from kb-server only
RUST_LOG=kb_server=debug ./kb-server

# Debug for kb-server plus surrealdb query noise
RUST_LOG=kb_server=debug,surrealdb=info ./kb-server

# Full trace for a specific module
RUST_LOG=kb_server::db::search=trace ./kb-server

# JSON structured output for log parsing
KB_LOG_JSON=1 RUST_LOG=kb_server=debug ./kb-server 2> /tmp/kb-debug.log
```

For the HTTP daemon, add to the systemd unit:
```ini
[Service]
Environment=RUST_LOG=kb_server=info
StandardError=journal
```

### Concrete changes to make

1. **Add startup INFO log** in `main.rs` after config loads:
   ```rust
   info!(
       version = env!("CARGO_PKG_VERSION"),
       transport = %transport_mode,
       db_path = %db_path.display(),
       "kb-server starting"
   );
   ```

2. **Log memory count at startup** after `connect()` succeeds:
   ```rust
   if let Ok(count) = memory::count(&db, Some(MemoryStatus::Active)).await {
       info!(active_memories = count, "database ready");
   }
   ```

3. **Add `#[instrument]` to all tool execute functions** with `skip` on content/summary/query:
   ```rust
   #[instrument(skip(db, embedding_client, input), fields(
       tool = "kb_remember",
       memory_type = ?input.memory_type,
       subtype = ?input.subtype,
       project = ?input.project
   ))]
   pub async fn execute(...) -> Result<RememberOutput> {
   ```

4. **Log dedup events at DEBUG** in `remember::execute`:
   ```rust
   Ok(dedup::DedupResult::Duplicate { existing_id, similarity }) => {
       tracing::debug!(existing_id, similarity, "dedup reject");
       return Err(...);
   }
   Ok(dedup::DedupResult::Warning { similar_id, similarity }) => {
       tracing::debug!(similar_id, similarity, "dedup near-duplicate warning");
       warning_msg = Some(...);
   }
   ```

5. **Log supersession at DEBUG** in `remember::execute`:
   ```rust
   Ok(true) => {
       tracing::debug!(new_id = %id, old_id = %old_id, "memory superseded");
       graph::link_memories(...).await?;
   }
   ```

6. **Remove entity name from `#[instrument]` span** in `entity::upsert`:
   ```rust
   #[instrument(skip(db, name, attrs, context), fields(kind = %kind))]
   ```

7. **Remove entity name from WARN** in `auto_link::match_known_entities`:
   ```rust
   Err(e) => warn!(id = %memory_id, "entity auto-link failed: {e}"),
   // Do not log `entity = name`
   ```

8. **Downgrade embedding model load failure** from ERROR to WARN in `embeddings.rs:68`.

9. **Downgrade Gemini primary failure** from WARN to DEBUG in `llm/client.rs:92` when Groq fallback succeeds.

10. **Add project/type/limit fields** to `search::recall` span:
    ```rust
    #[instrument(skip(db, query, query_vector), fields(
        memory_type = ?memory_type,
        project = ?project,
        limit
    ))]
    ```
