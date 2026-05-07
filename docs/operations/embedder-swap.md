# Embedder Swap Runbook

**Context:** ADR-016 fingerprint gate and ADR-015 (local model swap).

When the configured embedder changes (provider, model, or dimensions), smriti
refuses to start rather than silently corrupting vector recall with mismatched
embedding spaces. This runbook covers the zero-downtime recovery path.

---

## Symptom

`smriti.service` fails to start. Journal shows a fingerprint mismatch error:

```
journalctl -u smriti -f
```

Example output:
```
smriti[1234]: ERROR embedder fingerprint mismatch — stored: local/gte-large-en-v1.5-q/1024, current: voyage/voyage-4-large/1024
smriti[1234]: ERROR run kb_reembed to re-vectorise all active memories, then restart
```

BM25 recall still works against the live DB (smriti is simply not running), but
you cannot reach it until the process starts successfully.

---

## When This Happens

The fingerprint gate fires whenever the embedder configuration written to
`_kb_metadata` on the previous run differs from the current config:

| Trigger | Example |
|---|---|
| Switching provider | `local` → `voyage` or `voyage` → `local` |
| Switching local model | `gte-large-en-v1.5-q` → `bge-large-en-v1.5` |
| Changing output dimensions | `1024` → `256` |

It does NOT fire on a fresh database with no stored vectors.

---

## Recovery steps

### Step 1 — Enable the force-start escape valve

Edit `/etc/systemd/system/smriti.service` (or the EnvironmentFile it points to)
and add the escape-valve env var:

```ini
[Service]
Environment=SMRITI_FORCE_REEMBED_ON_START=1
```

If using an EnvironmentFile (e.g. `/etc/smriti/secrets.env`), add instead:

```
SMRITI_FORCE_REEMBED_ON_START=1
```

### Step 2 — Reload and restart the service

```bash
sudo systemctl daemon-reload
sudo systemctl restart smriti
```

Verify it started:

```bash
systemctl status smriti
```

The server will start with the new embedder configuration but the stored vectors
are stale. Vector recall returns degraded results; BM25 keeps full coverage.

### Step 3 — Trigger re-embedding via the librarian

From any Claude Code session, invoke the librarian subagent:

```
Agent({
  subagent_type: "smriti-librarian",
  prompt: "run kb_reembed — embedder was swapped, all active memories need re-vectorising"
})
```

The librarian calls `mcp__smriti__kb_reembed` directly. This is the only
sanctioned write path for this operation; do not call `kb_reembed` from the
main session.

Alternatively, if you have direct MCP tool access in a pinch:
```
kb_reembed()
```

### Step 4 — Monitor until completion

```bash
journalctl -u smriti -f
```

Wait for the log line indicating reembed completion, e.g.:
```
smriti[1234]: INFO reembed complete — 842 memories updated, 0 errors
```

Vector recall returns to full strength once this appears. Do not restart the
service during reembed — it will abort the in-progress batch.

### Step 5 — REMOVE the escape valve env var

**This step is mandatory.** Leaving `SMRITI_FORCE_REEMBED_ON_START=1` in place
means the next fingerprint mismatch (another model swap, a config typo) will
silently auto-proceed with mismatched vectors instead of refusing to start.

Remove the line you added in Step 1, then:

```bash
sudo systemctl daemon-reload
sudo systemctl restart smriti
```

### Step 6 — Verify the fingerprint is now current

Check the service started cleanly:

```bash
journalctl -u smriti --since "1 minute ago"
```

Expected output (no fingerprint mismatch error):
```
smriti[5678]: INFO embedder fingerprint OK — voyage/voyage-4-large/1024
smriti[5678]: INFO smriti ready on http://127.0.0.1:47821/mcp
```

---

## Why `kb_reembed` routes through the librarian

The global write protocol (CLAUDE.md) routes all MCP write calls through the
`smriti-librarian` subagent. `kb_reembed` is a write-side admin tool registered
in the librarian's `tools:` frontmatter (`mcp__smriti__kb_reembed`). Main
sessions are instructed not to call write tools directly, keeping the main
tool-list count below the Anthropic degradation threshold (~30–50 tools).

See `~/.claude/agents/smriti-librarian.md` and `~/.claude/CLAUDE.md` for the
full routing rationale.

---

## Switching from local to Voyage AI (specific sub-case)

Before starting the steps above, ensure Voyage prerequisites are met:

1. Add a payment method on the Voyage dashboard (paid-tier privacy contract —
   the free tier permits Voyage to train on inputs; disqualified for personal
   memory data per ADR-016).
2. Set `VOYAGE_API_KEY` in `/etc/smriti/secrets.env` (or `config.toml`).
3. In `/etc/smriti/config.toml`:
   ```toml
   [embeddings]
   provider = "voyage"
   model = "voyage-4-large"
   dimensions = 1024

   [embeddings.voyage]
   accepted_terms = true
   ```
4. Proceed with Steps 1–6 above.

To switch back to local, reverse the config and repeat Steps 1–6.

---

## Systemd service notes (ADR-028)

smriti uses `sd_notify` and systemd Watchdog support (ADR-028):
- The service emits `READY=1` after the HTTP listener binds (`Type=notify` in the unit).
- If `WatchdogSec=` is set in the unit, smriti spawns a ticker at `WatchdogUSec/2` to
  emit `WATCHDOG=1` on schedule. Missing ticks cause systemd to restart the process.
- During a reembed (`kb_reembed`), the process remains running and Watchdog ticks
  continue — do not stop the service mid-reembed.

## References

- ADR-015: Local model selection (`docs/decisions/015-embedder-swap.md`)
- ADR-016: Voyage AI backend + fingerprint gate policy (`docs/decisions/016-cloud-embedder-backend.md`)
- ADR-028: Observability rollout — sd-notify, Watchdog, request_id (`docs/decisions/028-observability-rollout.md`)
- Librarian agent: `~/.claude/agents/smriti-librarian.md`
- Global write protocol: `~/.claude/CLAUDE.md`
