# ADR-018: Voyage Batch API for Bench Ingest

**Status:** Accepted — Phase 3 (bench-only)
**Date:** 2026-04-30
**Extends:** ADR-016 (Voyage AI cloud embedder backend)

## Context

The synth bench harness currently ingests 1833 memories via sequential per-call
`/v1/embeddings` requests (~146 s ingest stage, synth bench r6). Voyage's Batch API
(`/v1/files` + `/v1/batches`) offers a 33% cost discount and bulk-submit semantics —
same model, same vectors, same quality, but asynchronous with up-to-12h completion SLA.

For bench runs that already take 20+ minutes in total, a 12h ingest-stage SLA is
acceptable, and the discount compounds over repeated bench iterations that are the
dominant development cost for retrieval-quality work.

ADR-016 noted this as a future path at the end of its "Reembed path" section. This ADR
scopes and closes that forward reference.

Production `kb_remember` cannot use the Batch API — the call is synchronous from the
MCP caller's perspective, embedding is a required pre-requisite for the dedup pre-check,
and a 12h wait would break the write contract entirely.

## Decision

Add `VoyageBatchClient` library code at `src/db/embeddings/voyage_batch.rs` (peer of
`voyage.rs`, sharing the same auth model and Voyage host). The bench harness opts in via
the `BENCH_USE_VOYAGE_BATCH=1` environment variable. When enabled, ingest collects all
documents into a single batch, uploads JSONL via `/v1/files`, submits via `/v1/batches`,
polls until complete, and downloads results.

The production write path never touches this module.

### Architecture

- New module `src/db/embeddings/voyage_batch.rs`.
- Public type `VoyageBatchClient` with three primary methods:
  - `submit_batch(documents: &[&str]) -> BatchHandle`
  - `await_completion(handle: &BatchHandle) -> BatchStatus`
  - `fetch_results(handle: &BatchHandle) -> Vec<Embedding>`
  - Plus convenience combinator `embed_documents_batch` chaining all three.
- `BatchHandle { file_id, batch_id }` is a public type so callers can persist it
  between submit and fetch. This enables idempotent restart: bench writes the handle to
  a checkpoint file before entering the poll loop; a crash mid-poll resumes from the
  checkpoint rather than re-submitting.
- Production write path (`kb_remember`, `kb_consolidate`, `chunking::chunk_and_embed`,
  `reembed_all`) is unchanged. `voyage_batch.rs` is wired only into
  `bench/src/synth_bench.rs`, gated by `BENCH_USE_VOYAGE_BATCH=1`.

### Why bench-only

| Constraint | Production `kb_remember` | Bench ingest |
|---|---|---|
| Caller waits for response | Yes — MCP round-trip | No — offline loop |
| Embedding needed now | Yes — dedup pre-check requires vector | No — stored after batch completes |
| Acceptable SLA | Sub-second | 20-60 min typical, 12h worst-case |
| 33% discount value | Marginal (few calls/day) | Meaningful (1833 × repeated runs) |

### Idempotent restart

`embed_documents_batch` (the convenience method) does NOT persist anything internally.
Bench wires its own checkpoint file — the library exposes `BatchHandle` deliberately so
the checkpoint is the caller's responsibility. This avoids building a generic checkpoint
system that is out of scope and keeps the library surface minimal.

A crash before the checkpoint file is written requires re-submitting; a crash after
allows resuming by calling `await_completion` and `fetch_results` with the saved handle.

### Polling

30-second default poll interval, ±20% jitter. 14h timeout cap (12h SLA + 2h safety
buffer). Progress logged at `tracing::info!` level.

## Alternatives Considered

| Alternative | Disposition |
|---|---|
| Use Batch API for production writes | Rejected — synchronous write contract; embedding required for dedup pre-check; 12h wait breaks `kb_remember`. |
| OpenAI Batch API | Rejected — different embedder and model; would mix embedding spaces; ADR-016 fingerprint gate would refuse to start. |
| Custom local job queue with sync embeddings | Rejected — no discount, just deferred latency; more infra to maintain. |
| Skip batch, absorb sync cost | Rejected — repeated bench iteration is the dominant development cost for retrieval-quality work; 33% over many runs is meaningful. |
| Wire batch into `kb_reembed` admin path | Deferred — useful for 100k+ memory backfills; out of scope here. Viable follow-up with no ADR-016/017 conflicts. |

## Consequences

### Positive

- 33% cost reduction on bench ingest embeddings (Voyage published discount).
- Single file upload + single batch poll replaces 1833 sequential `POST /v1/embeddings`
  calls.
- No production code changes — blast radius limited to bench harness.

### Negative — must accept

- Bench ingest goes from ~146 s (sync) to 20-60 min typical (12h worst-case batch SLA).
  Operator can revert to sync by unsetting `BENCH_USE_VOYAGE_BATCH`.
- Idempotent restart is the caller's responsibility. Bench must manage a checkpoint
  file; crash before checkpoint = re-submit = double charge on that run.
- Two additional Voyage API surfaces (`/v1/files`, `/v1/batches`) to track as Voyage
  evolves.

### Trade-offs accepted

- Longer ingest wall-clock time accepted in exchange for cost discount on iterative
  bench development.
- Checkpoint responsibility pushed to the caller (bench harness) rather than the
  library, keeping `VoyageBatchClient` surface minimal.

### Test coverage

- `voyage_batch_submit_uploads_jsonl_then_creates_batch` — wiremock asserts `/v1/files`
  then `/v1/batches` called in order.
- `voyage_batch_await_polls_until_completed` — wiremock chains `validating` →
  `in_progress` → `completed` status progression.
- `voyage_batch_fetch_returns_in_input_order` — wiremock returns out-of-order
  `custom_id`s; assert results reordered to match input order.
- `voyage_batch_fetch_count_mismatch` — wiremock returns N-1 results; assert error.
- `voyage_batch_failed_status_propagates` — wiremock returns `failed`; assert error
  message bubbles to caller.

## Migration notes

For bench operators:

1. Ensure `VOYAGE_API_KEY` is set (paid tier, same privacy gate as ADR-016).
2. Set `BENCH_USE_VOYAGE_BATCH=1` before running
   `cargo run --bin synth_bench --release`.
3. Bench prints the batch ID at submit time — persist this before the poll loop if
   you intend to support restart.
4. Expect 20-60 min ingest typical; worst-case 12h.
5. To revert to sync ingest: unset `BENCH_USE_VOYAGE_BATCH` (or set to `0`).

## Related

- ADR-016: Voyage AI cloud embedder backend (this ADR extends it; closes the
  forward-reference to "future ADR-018" in ADR-016's reembed-path section).
- ADR-017: Voyage reranker (also extends ADR-016; orthogonal — recall-side only).
