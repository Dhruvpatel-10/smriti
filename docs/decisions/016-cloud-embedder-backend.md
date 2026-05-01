# ADR-016: Voyage AI Cloud Embedder Backend

**Status:** Accepted — Phase 1 (foundations) shipped 2026-04-30
**Date:** 2026-04-30
**Supersedes (in part):** ADR-015 (local model selection remains valid as the offline fallback)

## Context

Local fastembed (currently `embeddinggemma-300m` MRL-truncated to 256 dims) gives
adequate quality for personal-scale recall but leaves measurable headroom on the
upstream MTEB benchmarks vs. cloud-tier models. The user explicitly accepted the
network-dependency tradeoff:

> "Don't over optimize I've good wifi and this system is used by claude or llm
> which needs wifi any ways. If quality diff is real then implement cloud."

Voyage AI's `voyage-4-large` is the strongest cost-quality fit:

| Property | Value |
|---|---|
| Native dims | 1024 (MRL truncatable to 256/512/2048) |
| Context | 32K tokens per input |
| MTEB | ~+8 pts over GTE-Large-Q on retrieval-relevant tasks |
| Asymmetric encoding | `input_type=query` vs. `document` (cross-encoder-like prompting) |
| Free tier | 200M tokens (≈45 yr personal heavy use, ≈170 full bench runs) |
| Paid tier (card on file) | $0.12 / 1M tokens, no training on inputs, no logging beyond ops |
| Free tier privacy | Trains on inputs — disqualified for personal-memory data |

The free-tier privacy gap is decisive: Voyage's no-training guarantee requires
a payment method on file regardless of whether usage stays under the free quota.
Without a card, Voyage retains the right to train. Smriti stores intimate
personal context, so we treat the paid-tier privacy contract as a hard
prerequisite and refuse startup unless the operator explicitly acknowledges it.

## Decision

Add Voyage AI as an opt-in second embedder backend alongside the existing local
fastembed path. Selection is per-config, not per-call:

```toml
[embeddings]
enabled = true
provider = "voyage"           # or "local" (default)
model = "voyage-4-large"      # ignored when provider=local
dimensions = 1024

[embeddings.voyage]
# api_key = "..."              # or set VOYAGE_API_KEY env var
model = "voyage-4-large"
output_dimension = 1024
request_timeout_ms = 30000
accepted_terms = true          # MANDATORY — see "Privacy gate" below
```

### Architecture

- `Embedder` is an enum with two variants — `Local(LocalEmbedder)` and
  `Voyage(VoyageEmbedder)`. We deliberately did NOT refactor to a `dyn Trait`
  because the blast radius is large for no concrete callsite gain at the present
  scale (one production user, two backends).
- The enum exposes asymmetric methods `embed_document(&str)` and
  `embed_query(&str)`. Local backend ignores the discriminator (symmetric model);
  Voyage maps it to `input_type` on the request.
- `embed()` is retained as a backward-compat shim that delegates to
  `embed_document()`. Recall paths (`kb_recall`, `kb_recall_at`) now call
  `embed_query()` explicitly so Voyage's asymmetric advantage is realised.
  Stored-document paths (`kb_remember` dedup vector, `kb_consolidate`,
  `chunking::chunk_and_embed`, `reembed_all`) call `embed_document()`.
- Voyage HTTP client uses `reqwest` with rustls (no system OpenSSL dep) and
  hand-rolled exponential backoff + jitter + `Retry-After` parsing for 429/5xx.
  Non-retryable 4xx (400/401/403/404) fail fast with the API-returned body.
- No startup token-burn validation. Bad keys, broken proxies, and revoked
  credentials surface on the first real embed call — burning a request per
  process boot under crash-loop scenarios was a real cost with no upside.

### Failure semantics

- **Recall path (`kb_recall`, `kb_recall_at`):** soft-fail on Voyage 5xx after
  retries. The query returns BM25-only results with a tracing warning. This
  honours ADR-005 — vector search has always been best-effort, network outages
  must not turn the personal memory daemon into a brick on a flight or in a
  café with bad wifi.
- **Write path (`kb_remember`):** hard-fail on Voyage error after retries —
  storing a memory without a vector silently degrades future recall, and the
  user just gave us their input synchronously, so a clean error is the right
  signal. Existing dedup pre-check already requires a vector to function.
- **No silent fallback to local:** mixing 1024-dim Voyage vectors with 256-dim
  local fastembed vectors in the same DB produces meaningless cosine scores.
  We refuse this by gate (see "Fingerprint gate") rather than papering over
  with implicit fallback.

### Privacy gate

`embeddings.voyage.accepted_terms` is a required boolean. Setting
`provider = "voyage"` without `accepted_terms = true` raises a startup
`KbError::Config` directing the operator to either set a payment method on
the Voyage dashboard (paid tier semantics, even if usage stays under the free
quota) or revert to `provider = "local"`.

This is an attestation, not enforcement — we have no way to verify a card is
attached. But the gate makes the privacy decision explicit at config time
rather than letting an operator drift into the training-eligible free tier
unknowingly.

### Fingerprint gate (Migration 015)

A new `_kb_metadata` table stores the embedder fingerprint
`provider/model/dimensions` of the embedder that wrote the existing vectors.
On startup, after migrations and after `Embedder` construction, we compare
the persisted fingerprint to the current configuration:

| Stored | Current | Vectors exist? | Action |
|---|---|---|---|
| (none) | any | — | write current, proceed |
| equal | equal | — | proceed |
| differ | differ | yes | **refuse to start** with actionable error → run `kb_reembed` |
| differ | differ | no | warn, write current, proceed (fresh DB) |

This catches three real footguns:

1. Operator switches `provider = local → voyage` without re-embedding → query
   vectors live in 1024-dim space, stored vectors in 256-dim space → silent
   garbage recall.
2. Operator changes `voyage.output_dimension` from 1024 → 256 → same garbage
   silently.
3. Operator changes local `model` (e.g. embeddinggemma → gte-large-q) → same
   garbage silently.

The check runs once at startup, not per-query — zero hot-path cost.

### Reembed path

`reembed_all` is the recovery procedure. Existing implementation iterates
active memories and re-embeds via `embedder.embed_document()`. Now backed by
the `Embedder` enum, so it works for either backend with no further change.
After completion, the operator must restart the server so the fingerprint
gate re-runs and writes the new identity.

ADR-018 covers Voyage Batch API for bench ingest specifically. A future admin
`kb_reembed_batch` path may build on the same `VoyageBatchClient` but is not
yet scoped.

## Alternatives Considered

| Alternative | Disposition |
|---|---|
| `dyn Embedder` trait refactor | Rejected — large blast radius (every callsite re-typed, dynamic dispatch overhead at hot paths) for no current callsite gain with two backends |
| Silent fallback to local on Voyage outage | Rejected — mixes embedding spaces, cosine similarity becomes meaningless, recall returns garbage |
| Hard-fail recall on Voyage outage | Rejected — violates ADR-005's best-effort vector contract; turns flights into bricked product |
| OpenAI `text-embedding-3-large` | Deferred — comparable quality, similar pricing, no asymmetric input_type, no batch API |
| Cohere `embed-english-v3.0` | Deferred — similar tier; smaller MTEB lift than Voyage in retrieval; revisit if Voyage degrades |
| Free Gemini Embedding 2 | Rejected — Google trains on free-tier inputs (TOS), disqualified for personal memory data |
| Startup API ping for key validation | Rejected — burns one token per process boot, blocks init 200-800ms, fails opaquely behind enterprise HTTPS proxies, gives no functional benefit over a clear 401 on first real call |
| API key in `config.toml` only | Rejected — env var (`VOYAGE_API_KEY`) takes precedence; secrets do not belong in checked-in config |

## Consequences

### Positive

- ~+8 MTEB lift on retrieval-relevant tasks via `voyage-4-large`.
- Asymmetric `input_type=query|document` exploited end-to-end at recall time.
- 32K context allows embedding entire conversation transcripts as documents
  without the 256-token clip the local backend imposes.
- 1024-native dims with MRL truncation to 2048 available if quality demands rise.
- Privacy and fingerprint gates make embedder-config drift fail loudly at
  startup instead of silently corrupting recall.

### Negative — must accept

- Network dependency on the recall path. Local fast-path is gone when
  `provider = voyage`. Mitigated by ADR-005 BM25 fallback on 5xx, but per-query
  latency now includes a cloud round-trip (~80-200 ms on home wifi vs. ~10-50
  ms local ONNX).
- Cost telemetry not yet wired (deferred to a follow-up). Free-tier usage at
  personal scale puts this firmly in "monitor, don't optimise" territory.
- Dedup thresholds (0.92 reject, 0.82 warn at `src/db/dedup.rs:10-11`) were
  calibrated on fastembed cosine distribution. Voyage's `voyage-4-large` cosine
  space is different — identical-text similarity may shift by 0.02-0.05. After
  switching, watch for false-positive dedup rejections; recalibrate empirically
  after ~50 writes if drift appears. Out of scope for this ADR.
- `.fastembed_cache/` (~800 MB ONNX model) is unmanaged on switch — operator
  must clean up manually if disk is constrained.

### Test coverage

- `Embedder::Voyage` request/response serialisation: `voyage::tests` (4 cases)
- Retry classification (429/5xx vs. 401/400/404): `voyage::tests::retry_classification_correct`
- Backoff growth: `voyage::tests::backoff_grows_exponentially`
- Fingerprint encode/decode roundtrip: `embeddings::fingerprint::tests` (4 cases)
- Recall paths use `embed_query()`: not yet directly asserted (test infra
  passes `None` for embedder); follow-up to add a wiremock-backed integration
  test asserting `input_type=query` in the request body.

## Migration notes for existing operators

Existing local-fastembed users are unaffected — defaults remain
`provider = "local"`. To switch to Voyage:

1. Add a payment method on the Voyage dashboard (paid-tier privacy contract).
2. `export VOYAGE_API_KEY=...` (or set in `config.toml`).
3. Set `provider = "voyage"` and `accepted_terms = true` in
   `[embeddings.voyage]`.
4. Restart the server. Startup will fail with a fingerprint mismatch error
   listing the previous and current configurations.
5. Run `kb_reembed` (admin tool / MCP call) to re-vectorise all active
   memories with Voyage. Vector recall returns degraded results during the
   re-embed window; BM25 keeps full coverage.
6. Restart the server. Fingerprint gate now writes the Voyage fingerprint and
   proceeds. Vector recall returns to full strength.

To switch back to local: same procedure in reverse.

## Migration Path — zero-downtime embedder swap runbook

The six-step summary above is intentionally brief. The full actionable runbook
(copy-pasteable commands, systemd env-var edit, escape-valve removal, librarian
invocation for `kb_reembed`) lives at:

```
docs/operations/embedder-swap.md
```

Key points not captured in the six steps above:

- `kb_reembed` must be invoked **through the `smriti-librarian` subagent**
  (global write-path protocol, `~/.claude/CLAUDE.md`). The tool is registered
  in the librarian's `tools:` frontmatter as `mcp__smriti__kb_reembed`. Main
  sessions must not call it directly.
- The `SMRITI_FORCE_REEMBED_ON_START=1` escape valve must be **removed** after
  re-embedding is confirmed complete. Leaving it in place bypasses the
  fingerprint gate permanently, defeating its purpose.
- Do not restart smriti mid-reembed — in-progress batches abort and partial
  re-embedding leaves a mixed-space vector store.
