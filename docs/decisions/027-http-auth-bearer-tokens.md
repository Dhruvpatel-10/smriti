# ADR-027: HTTP Daemon Bearer-Token Authentication

**Date:** 2026-05-01
**Status:** Accepted
**Extends:** ADR-002 (HTTP daemon transport)

## Context

Smriti runs as a system-level HTTP daemon (`127.0.0.1:47821`) shared by two users
(`stark` and `leanscale`) on the same host. Prior to this ADR, **any local process**
could issue `kb_recall` (or any other tool call) and read all Standard-visibility
memories regardless of which user wrote them. This is an access-control gap for a
multi-user host.

The threat model is: a malicious or compromised local process belonging to one user
reading memories written by the other. Loopback (`127.0.0.1`) already prevents
network-remote attackers; this ADR closes the local cross-user gap.

## Decision

Add **pre-shared bearer tokens**, one per user, enforced at the HTTP layer before
any MCP content is processed.

### Token file format

`/etc/smriti/tokens.toml` — mode 0640, owned by `root:smriti`:

```toml
[tokens]
stark     = "tok_<openssl rand -hex 32>"
leanscale = "tok_<openssl rand -hex 32>"
```

The key is a human-readable identity name; the value is the secret token.

### Validation rules (enforced at startup)

- Token length ≥ 32 characters (minimum entropy).
- Characters restricted to `[a-zA-Z0-9-_]` (hex output of `openssl rand -hex 32`
  satisfies this without quoting).
- Parse failure → server refuses to start (fatal).

### Auth-disabled mode

When the tokens file is missing or contains an empty `[tokens]` table:
- Server logs a `warn!` at startup: "auth disabled — anyone on localhost can read all
  memories".
- All requests are accepted (useful for single-user dev setups).

This is the default for existing single-user installs — no breaking change.

### HTTP enforcement

A `AuthGuard<S>` Tower/Hyper service wrapper intercepts every request before the rmcp
`StreamableHttpService` is invoked. It reads `Authorization: Bearer <token>`, looks up
the token in the in-memory map, and either:
- Forwards the request to the inner service (valid token or auth-disabled), or
- Returns `401 Unauthorized` with `WWW-Authenticate: Bearer realm="smriti"` immediately.

Token values are **never logged** — only the resolved identity name is emitted at
`debug!` level.

### Per-user memory filtering

This ADR only adds authentication (who you are). Per-user authorization (what you can
see) is a separate concern deferred to a future ADR. After this change all
authenticated users can still read each other's Standard memories — the auth layer
provides an audit trail via identity logging, and is the prerequisite for scoped
filtering.

## Implementation

- `src/config.rs`: `AuthConfig` struct under `[server.auth]`; `tokens_file` field
  (default `/etc/smriti/tokens.toml`).
- `src/auth.rs`: `load_tokens()`, `validate_tokens()`, `check_bearer()`, `TokenMap`
  newtype.
- `src/main.rs`: `AuthGuard<S>` Hyper service wrapper; tokens loaded once at startup
  in `run_http`.
- `src/lib.rs`: `pub mod auth` exported.
- `scripts/install-system.sh`: generates tokens via `openssl rand -hex 32` and writes
  `/etc/smriti/tokens.toml` (mode 0640, `root:smriti`).
- `docs/config.md`: documents `[server.auth]` section and MCP registration with
  `--header`.

## MCP Client Registration (per-user, run once)

```sh
claude mcp add smriti --transport http http://127.0.0.1:47821/mcp \
  --header "Authorization: Bearer <your-token>" -s user
```

Each user adds their own token from `/etc/smriti/tokens.toml`.

## Consequences

- Local process isolation: cross-user reads now require possession of the target
  user's token.
- Zero operational impact on single-user installs (auth-disabled mode).
- Token rotation: edit `/etc/smriti/tokens.toml` and `systemctl restart smriti` — no
  DB changes needed.
- Future per-user memory scoping can use the resolved identity already injected by
  `AuthGuard`.
