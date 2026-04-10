# ADR-002: HTTP Daemon Transport for Multi-Session

**Status:** Accepted  
**Date:** 2026-04-08  
**Context:** SurrealKV locks data files at the OS level. When two Claude Code sessions call `kb_remember` simultaneously via stdio transport, the second process fails to open the DB.

## Decision

Add HTTP Streamable transport as an alternative to stdio. One long-running daemon serves all sessions via `127.0.0.1:3001`.

## Implementation

- `KB_TRANSPORT=http` env var selects HTTP mode (default: stdio for backward compat).
- Uses rmcp's `StreamableHttpService` + `hyper-util` for HTTP/1.1 + HTTP/2.
- `LocalSessionManager` handles per-session state.
- systemd user service (`scripts/kb-server.service`) for auto-start.

## Rationale

- **One DB connection:** All sessions share a single `SurrealClient` (which is `Arc`-based internally).
- **No file lock contention:** Only one process opens SurrealKV.
- **SSE support:** Streamable HTTP transport supports server-sent events for efficient long-polling.

## Tradeoffs

- Requires a running daemon (vs. stdio's zero-setup).
- Localhost only (127.0.0.1) — no network exposure.
- Session management adds slight complexity.

## Consequences

- `~/.claude/settings.json` points to `http://127.0.0.1:3001/mcp` in daemon mode.
- stdio mode remains for single-session dev/testing.
