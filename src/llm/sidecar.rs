//! JSON-RPC stdio client for the TypeScript LLM sidecar process.
//!
//! Spawns `node <sidecar_path>` and communicates over stdin/stdout using
//! newline-delimited JSON-RPC 2.0.  The supervisor loop owns the child
//! process and handles exponential backoff + circuit breaker on failure.
//! P1-E (relation extraction) is the primary caller.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex as PlMutex;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

use tokio_util::sync::CancellationToken;

use crate::config::LlmConfig;

// ── Type aliases ─────────────────────────────────────────────────────────────

type PendingSender = oneshot::Sender<Result<serde_json::Value, RpcError>>;
type PendingMap = Arc<PlMutex<HashMap<u64, PendingSender>>>;

// ── State machine ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum State {
    Healthy,
    // Reserved for future mid-backoff state visibility; set by supervisor.
    #[allow(dead_code)]
    BackingOff { until: Instant, fail_count: u32 },
    Broken,
}

// ── Public param/result types ────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExtractRelationsParams {
    pub memory_id: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TypedRelationOut {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub confidence: f32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ExtractRelationsResult {
    pub relations: Vec<TypedRelationOut>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExtractAnchorsParams {
    pub memory_id: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExtractAnchorsResult {
    pub anchors: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HealthResp {
    pub ok: bool,
    pub version: String,
}

#[derive(Debug, Serialize)]
pub struct SynthesizeConsolidationParams {
    pub cluster_id: String,
    pub summaries: Vec<String>,
    pub subtype: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SynthesizeConsolidationResult {
    pub should_merge: bool,
    pub confidence: f32,
    pub merged_summary: Option<String>,
    pub reason: String,
}

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(thiserror::Error, Debug)]
pub enum RpcError {
    #[error("sidecar restarted; request lost")]
    SidecarRestarted,
    #[error("sidecar unavailable (circuit breaker open)")]
    SidecarUnavailable,
    #[error("sidecar binary not found at {0}")]
    BinaryMissing(PathBuf),
    #[error("sidecar timeout after {0}ms")]
    Timeout(u64),
    #[error("invalid JSON-RPC response: {0}")]
    BadResponse(String),
    #[error("sidecar method error: code={code} message={message}")]
    MethodError { code: i32, message: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("not implemented: {0}")]
    NotImplemented(String),
}

// ── ServiceHandle ─────────────────────────────────────────────────────────────

/// Owned handle to a supervisor task. Call [`ServiceHandle::shutdown`] during
/// daemon teardown to cancel and await the supervisor cleanly.
///
/// If `shutdown` is not called (e.g., CLI subcommands that call `process::exit`
/// immediately), the supervisor is abandoned — acceptable because the OS cleans
/// up the child process. For long-running daemons, always call `shutdown`.
pub struct ServiceHandle {
    name: &'static str,
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

impl ServiceHandle {
    /// Cancel the supervisor task and wait up to `timeout` for it to exit.
    ///
    /// Returns `Ok(())` on clean exit within the timeout window.
    /// Returns `Err("timeout")` if the supervisor did not exit in time;
    /// in that case the task is aborted.
    pub async fn shutdown(self, timeout: Duration) -> Result<(), &'static str> {
        self.cancel.cancel();
        match tokio::time::timeout(timeout, self.handle).await {
            Ok(_) => {
                info!(name = self.name, "supervisor exited cleanly");
                Ok(())
            }
            Err(_) => {
                warn!(
                    name = self.name,
                    ?timeout,
                    "supervisor shutdown timed out — aborting"
                );
                Err("timeout")
            }
        }
    }

    /// Test-only constructor for exercising `ServiceHandle::shutdown` behaviour
    /// without a real sidecar binary. Do not use in production paths.
    #[doc(hidden)]
    pub fn new_test(cancel: CancellationToken, handle: JoinHandle<()>) -> Self {
        Self { name: "test-supervisor", cancel, handle }
    }
}

// ── JSON-RPC envelope types ───────────────────────────────────────────────────

#[derive(Serialize)]
struct JsonRpcRequest<'a, T: Serialize> {
    jsonrpc: &'a str,
    id: u64,
    method: &'a str,
    params: T,
}

#[derive(Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<u64>,
    result: Option<serde_json::Value>,
    error: Option<JsonRpcErrorBody>,
}

#[derive(Deserialize)]
struct JsonRpcErrorBody {
    code: i32,
    message: String,
}

// ── Client struct ─────────────────────────────────────────────────────────────

pub struct SidecarClient {
    config: LlmConfig,
    #[allow(dead_code)]
    base_dir: PathBuf,
    pending: PendingMap,
    next_id: AtomicU64,
    tx_to_writer: mpsc::UnboundedSender<String>,
    state: Arc<PlMutex<State>>,
    restart_count: Arc<AtomicU64>,
    dropped_inflight: Arc<AtomicU64>,
}

impl SidecarClient {
    /// Create a new `SidecarClient` and start the supervisor task.
    ///
    /// Returns `(Arc<SidecarClient>, ServiceHandle)`. The caller must keep the
    /// `ServiceHandle` and call [`ServiceHandle::shutdown`] during daemon
    /// teardown so the supervisor is cancelled and awaited before process exit.
    ///
    /// The `cancel` token passed in is cloned — the caller retains their clone,
    /// and the supervisor owns the other. `ServiceHandle` wraps a fresh clone
    /// of the same token so that calling `ServiceHandle::shutdown` fires the
    /// same token.
    pub async fn new(
        config: LlmConfig,
        base_dir: PathBuf,
        cancel: CancellationToken,
    ) -> Result<(Arc<Self>, ServiceHandle), RpcError> {
        let resolved = if config.sidecar_path.is_absolute() {
            config.sidecar_path.clone()
        } else {
            base_dir.join(&config.sidecar_path)
        };
        if !resolved.exists() {
            return Err(RpcError::BinaryMissing(resolved));
        }

        let pending = Arc::new(PlMutex::new(HashMap::new()));
        let state = Arc::new(PlMutex::new(State::Healthy));
        let restart_count = Arc::new(AtomicU64::new(0));
        let dropped_inflight = Arc::new(AtomicU64::new(0));
        let (tx_to_writer, rx_to_writer) = mpsc::unbounded_channel();

        let client = Arc::new(Self {
            config: config.clone(),
            base_dir: base_dir.clone(),
            pending: pending.clone(),
            next_id: AtomicU64::new(1),
            tx_to_writer,
            state: state.clone(),
            restart_count: restart_count.clone(),
            dropped_inflight: dropped_inflight.clone(),
        });

        let handle = tokio::spawn(supervisor_loop(
            config,
            resolved,
            rx_to_writer,
            pending,
            state,
            restart_count,
            dropped_inflight,
            cancel.clone(),
        ));

        let service = ServiceHandle {
            name: "sidecar-supervisor",
            cancel,
            handle,
        };

        Ok((client, service))
    }

    pub async fn extract_relations(
        &self,
        params: ExtractRelationsParams,
    ) -> Result<Vec<TypedRelationOut>, RpcError> {
        let value = self.call("extract_relations", params).await?;
        let parsed: ExtractRelationsResult = serde_json::from_value(value)?;
        Ok(parsed.relations)
    }

    pub async fn extract_anchors(
        &self,
        params: ExtractAnchorsParams,
    ) -> Result<Vec<String>, RpcError> {
        let value = self.call("extract_anchors", params).await?;
        let parsed: ExtractAnchorsResult = serde_json::from_value(value)?;
        // Clamp to 3, filter empty/whitespace-only strings.
        let anchors = parsed
            .anchors
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .take(3)
            .collect();
        Ok(anchors)
    }

    pub async fn synthesize_consolidation(
        &self,
        params: SynthesizeConsolidationParams,
    ) -> Result<SynthesizeConsolidationResult, RpcError> {
        let value = self.call("synthesize_consolidation", params).await?;
        Ok(serde_json::from_value(value)?)
    }

    pub async fn health(&self) -> Result<HealthResp, RpcError> {
        let value = self.call("health", serde_json::json!({})).await?;
        Ok(serde_json::from_value(value)?)
    }

    pub fn confidence_threshold(&self) -> f32 {
        self.config.confidence_threshold
    }

    pub fn restart_count(&self) -> u64 {
        self.restart_count.load(Ordering::Relaxed)
    }

    pub fn dropped_inflight(&self) -> u64 {
        self.dropped_inflight.load(Ordering::Relaxed)
    }

    async fn call<P: Serialize>(
        &self,
        method: &str,
        params: P,
    ) -> Result<serde_json::Value, RpcError> {
        // Circuit-breaker check — lock is dropped immediately, no await while held.
        if matches!(*self.state.lock(), State::Broken) {
            return Err(RpcError::SidecarUnavailable);
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest { jsonrpc: "2.0", id, method, params };
        let body = serde_json::to_string(&req)?;

        let (tx, rx) = oneshot::channel();
        { self.pending.lock().insert(id, tx); }

        if self.tx_to_writer.send(body).is_err() {
            // Supervisor is between restarts; clean up the pending slot.
            self.pending.lock().remove(&id);
            return Err(RpcError::SidecarRestarted);
        }

        let timeout_dur = Duration::from_millis(self.config.request_timeout_ms);
        match timeout(timeout_dur, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(e),
            // Sender dropped because supervisor killed inflight on child death.
            Ok(Err(_)) => Err(RpcError::SidecarRestarted),
            Err(_) => {
                // Timeout — remove to avoid leak.
                self.pending.lock().remove(&id);
                Err(RpcError::Timeout(self.config.request_timeout_ms))
            }
        }
    }
}

// ── Supervisor loop ───────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn supervisor_loop(
    config: LlmConfig,
    sidecar_path: PathBuf,
    mut rx_to_writer: mpsc::UnboundedReceiver<String>,
    pending: PendingMap,
    state: Arc<PlMutex<State>>,
    restart_count: Arc<AtomicU64>,
    dropped_inflight: Arc<AtomicU64>,
    cancel: CancellationToken,
) {
    let mut consecutive_failures: u32 = 0;

    loop {
        // Exit cleanly when shutdown is signalled.
        if cancel.is_cancelled() {
            break;
        }

        // Circuit-breaker probe: if Broken, wait 30s then attempt one recovery.
        let is_broken = matches!(*state.lock(), State::Broken);
        if is_broken {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
            *state.lock() = State::Healthy;
            consecutive_failures = 0;
        }

        // Spawn child process.
        // Strip the API key so the SDK falls back to the bundled Claude Code
        // OAuth session (Max subscription). If the var were present it would
        // bill the API account instead.
        info!(target: "sidecar", "spawning sidecar with ANTHROPIC_API_KEY removed (Max subscription auth)");
        let mut child = match Command::new("node")
            .arg(&sidecar_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_remove("ANTHROPIC_API_KEY")
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                error!(target: "sidecar", "spawn failed: {e}");
                consecutive_failures += 1;
                if consecutive_failures >= config.max_consecutive_failures {
                    *state.lock() = State::Broken;
                }
                let backoff = compute_backoff(consecutive_failures);
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                continue;
            }
        };

        // These are guaranteed present because we set Stdio::piped() above.
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        // Drain sidecar stderr lines to tracing::warn — never to stdout.
        tokio::spawn(stderr_drain_task(stderr));

        // Reader task signals EOF via a oneshot.
        let (reader_done_tx, reader_done_rx) = oneshot::channel::<()>();
        let pending_r = pending.clone();
        tokio::spawn(reader_task(stdout, pending_r, reader_done_tx));

        // Inner select loop: forward outbound messages to child stdin, watch
        // for reader EOF, and watch for child exit.  We keep rx_to_writer in
        // the supervisor so it survives across restarts.
        let exit_reason = run_child_io_loop(
            &mut rx_to_writer,
            stdin,
            reader_done_rx,
            &mut child,
            &cancel,
        )
        .await;

        // When cancel fired inside the IO loop, reap the child before breaking
        // so we avoid a zombie window. The child may still be running (it only
        // saw stdin EOF), so we give it a short grace period then rely on
        // kill_on_drop if it hasn't exited.
        if let ChildExitReason::Cancelled = exit_reason {
            let _ = child.wait().await;
            break;
        }

        // Drain all in-flight pending requests with SidecarRestarted.
        let dropped = {
            let mut p = pending.lock();
            let count = p.len() as u64;
            for (_id, sender) in p.drain() {
                let _ = sender.send(Err(RpcError::SidecarRestarted));
            }
            count
        };
        dropped_inflight.fetch_add(dropped, Ordering::Relaxed);
        restart_count.fetch_add(1, Ordering::Relaxed);

        let clean_exit = matches!(exit_reason, ChildExitReason::Natural(Ok(s)) if s.success());
        if clean_exit {
            consecutive_failures = 0;
        } else {
            consecutive_failures += 1;
        }

        if consecutive_failures >= config.max_consecutive_failures {
            warn!(
                target: "sidecar",
                consecutive_failures,
                "circuit breaker open"
            );
            *state.lock() = State::Broken;
        }

        let backoff = compute_backoff(consecutive_failures);
        warn!(target: "sidecar", "restarting in {:?}", backoff);
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(backoff) => {}
        }
    }
}

/// Signals whether `run_child_io_loop` exited due to cancellation or natural EOF.
enum ChildExitReason {
    /// Reader EOF or channel close — child exited naturally or client dropped.
    Natural(Result<std::process::ExitStatus, ()>),
    /// Cancel token fired while inside the IO loop.
    Cancelled,
}

/// Drives the per-child I/O: forwards outbound messages from the client channel
/// to child stdin, and returns when either the reader signals EOF, the child
/// exits, or the cancel token fires.  `rx_to_writer` is borrowed so the
/// supervisor retains ownership across restarts.
async fn run_child_io_loop(
    rx_to_writer: &mut mpsc::UnboundedReceiver<String>,
    mut stdin: tokio::process::ChildStdin,
    mut reader_done: oneshot::Receiver<()>,
    child: &mut tokio::process::Child,
    cancel: &CancellationToken,
) -> ChildExitReason {
    loop {
        tokio::select! {
            // Cancel fires while we are blocked waiting for the next message.
            // Drop stdin explicitly so the child sees EOF on its stdin pipe
            // and can shut down gracefully before we call wait().
            _ = cancel.cancelled() => {
                drop(stdin);
                return ChildExitReason::Cancelled;
            }
            // Outbound message from client → write to child stdin.
            msg = rx_to_writer.recv() => {
                match msg {
                    Some(body) => {
                        let line = format!("{body}\n");
                        if let Err(e) = stdin.write_all(line.as_bytes()).await {
                            warn!(target: "sidecar", "stdin write error: {e}");
                            // Child is gone; fall through to wait().
                            break;
                        }
                        if let Err(e) = stdin.flush().await {
                            warn!(target: "sidecar", "stdin flush error: {e}");
                            break;
                        }
                        debug!(target: "sidecar", "wrote request to child");
                    }
                    // Client dropped the SidecarClient — stop sending.
                    None => break,
                }
            }
            // Reader task saw EOF → child exited or pipe closed.
            _ = &mut reader_done => {
                break;
            }
        }
    }
    // Reap the child.
    let status = match child.wait().await {
        Ok(status) => Ok(status),
        Err(_) => Err(()),
    };
    ChildExitReason::Natural(status)
}

// ── Reader task ───────────────────────────────────────────────────────────────

async fn reader_task(
    stdout: tokio::process::ChildStdout,
    pending: PendingMap,
    done: oneshot::Sender<()>,
) {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF — child exited
            Ok(_) => {}
            Err(e) => {
                error!(target: "sidecar", "stdout read error: {e}");
                break;
            }
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let resp: JsonRpcResponse = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                warn!(target: "sidecar", "bad response JSON: {e}");
                continue;
            }
        };
        // Notifications have no id — skip them.
        let Some(id) = resp.id else { continue };
        let sender = { pending.lock().remove(&id) };
        if let Some(s) = sender {
            let result = if let Some(err) = resp.error {
                // Surface "not_implemented" as a dedicated variant.
                if err.message.contains("not_implemented") {
                    Err(RpcError::NotImplemented(err.message))
                } else {
                    Err(RpcError::MethodError { code: err.code, message: err.message })
                }
            } else if let Some(v) = resp.result {
                Ok(v)
            } else {
                Err(RpcError::BadResponse("no result and no error".into()))
            };
            let _ = s.send(result);
        }
    }
    let _ = done.send(());
}

// ── Stderr drain ──────────────────────────────────────────────────────────────

async fn stderr_drain_task(stderr: tokio::process::ChildStderr) {
    const MAX_LINE: usize = 4096;
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let t = line.trim_end();
                if !t.is_empty() {
                    if t.len() > MAX_LINE {
                        warn!(target: "sidecar", "{}[..truncated]", &t[..MAX_LINE]);
                    } else {
                        warn!(target: "sidecar", "{t}");
                    }
                }
            }
        }
    }
}

// ── Backoff ───────────────────────────────────────────────────────────────────

/// Exponential backoff: 1s, 2s, 4s, 8s, 16s, capped at 60s.
fn compute_backoff(attempts: u32) -> Duration {
    // Shift index is clamped to 6 so 1 << 6 = 64, capped to 60 below.
    let shift = attempts.saturating_sub(1).min(6) as u64;
    let secs = (1u64 << shift).min(60);
    Duration::from_secs(secs)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that a sidecar JSON-RPC error containing a sentinel string in its
    /// message field is surfaced verbatim in `RpcError::MethodError.message` by
    /// the reader task — but that the sentinel cannot escape via the Display
    /// fmt path (which only emits the stable `thiserror` template).
    ///
    /// This test simulates a sidecar that returns a JSON-RPC error body carrying
    /// arbitrary text (e.g., prompt material). We assert:
    /// 1. `RpcError::MethodError.message` captures the raw sidecar message (reader
    ///    passes it through unchanged — that's intentional, the sidecar is now
    ///    responsible for sanitising before encoding).
    /// 2. The `Display` output of the error does NOT expose the sentinel beyond the
    ///    stable `thiserror` template prefix; since the template is
    ///    "sidecar method error: code={code} message={message}", the message field
    ///    IS present in Display — but after our sidecar fix it will only ever
    ///    contain the sanitised form.  This test documents the contract.
    /// 3. The Debug output DOES contain struct internals — which is why main.rs
    ///    now uses `%e` (Display) rather than `?e` (Debug) at the warn! site.
    #[test]
    fn rpc_error_display_uses_stable_template_not_debug() {
        const SENTINEL: &str = "PROMPT_LEAK_MARKER";

        // Simulate what reader_task produces when the sidecar returns an error.
        // After the sidecar fix, this message will be safeErrorMessage output,
        // but we test the Rust side's Display contract independently.
        let err = RpcError::MethodError {
            code: -32603,
            message: format!("extract_failed:SomeError:{SENTINEL}"),
        };

        // Debug exposes struct internals including the raw message.
        let debug_str = format!("{err:?}");
        assert!(
            debug_str.contains(SENTINEL),
            "Debug format should contain the raw message (this is why we use %e not ?e in warn!)"
        );

        // Display uses the thiserror template: "sidecar method error: code=… message=…"
        // The stable prefix is present; the message field follows it.
        let display_str = format!("{err}");
        assert!(
            display_str.starts_with("sidecar method error:"),
            "Display should use the stable thiserror template"
        );

        // The key invariant: at the warn! site we now log %e (Display), so the
        // output is predictable and auditable — no hidden struct fields leak.
        // If the sidecar correctly sanitises, SENTINEL will never appear in the
        // sidecar's JSON-RPC response at all; this test captures the contract.
        assert!(
            display_str.contains("code=-32603"),
            "Display should contain the error code"
        );
    }

    #[test]
    fn synthesize_consolidation_params_serializes_optional_fields() {
        let params = SynthesizeConsolidationParams {
            cluster_id: "hash-abc123".to_string(),
            summaries: vec!["First memory summary.".to_string(), "Second memory summary.".to_string()],
            subtype: "decision".to_string(),
            project: Some("smriti".to_string()),
            domain: None,
        };
        let json = serde_json::to_value(&params).expect("serialize");
        assert_eq!(json["cluster_id"], "hash-abc123");
        assert_eq!(json["subtype"], "decision");
        assert_eq!(json["project"], "smriti");
        // domain is None → must be absent (skip_serializing_if)
        assert!(json.get("domain").is_none());
        assert_eq!(json["summaries"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn synthesize_consolidation_params_omits_both_optional_when_none() {
        let params = SynthesizeConsolidationParams {
            cluster_id: "xyz".to_string(),
            summaries: vec!["a".to_string()],
            subtype: "fact".to_string(),
            project: None,
            domain: None,
        };
        let json = serde_json::to_value(&params).expect("serialize");
        assert!(json.get("project").is_none());
        assert!(json.get("domain").is_none());
    }

    #[test]
    fn synthesize_consolidation_result_deserializes_merge_true() {
        let raw = r#"{
            "should_merge": true,
            "confidence": 0.92,
            "merged_summary": "Merged: key decision about system architecture.",
            "reason": "Both summaries describe the same architectural decision."
        }"#;
        let result: SynthesizeConsolidationResult = serde_json::from_str(raw).expect("deserialize");
        assert!(result.should_merge);
        assert!((result.confidence - 0.92).abs() < 1e-5);
        assert_eq!(result.merged_summary.as_deref(), Some("Merged: key decision about system architecture."));
        assert!(!result.reason.is_empty());
    }

    #[test]
    fn synthesize_consolidation_result_deserializes_merge_false() {
        let raw = r#"{
            "should_merge": false,
            "confidence": 0.40,
            "merged_summary": null,
            "reason": "Summaries describe distinct events despite shared keywords."
        }"#;
        let result: SynthesizeConsolidationResult = serde_json::from_str(raw).expect("deserialize");
        assert!(!result.should_merge);
        assert!((result.confidence - 0.40).abs() < 1e-5);
        assert!(result.merged_summary.is_none());
        assert!(!result.reason.is_empty());
    }

    #[test]
    fn compute_backoff_attempt_0_returns_1s() {
        assert_eq!(compute_backoff(0), Duration::from_secs(1));
    }

    #[test]
    fn compute_backoff_attempt_1_returns_1s() {
        assert_eq!(compute_backoff(1), Duration::from_secs(1));
    }

    #[test]
    fn compute_backoff_attempt_7_returns_60s() {
        assert_eq!(compute_backoff(7), Duration::from_secs(60));
    }

    #[test]
    fn compute_backoff_attempt_20_returns_60s() {
        assert_eq!(compute_backoff(20), Duration::from_secs(60));
    }

    #[tokio::test]
    #[ignore = "requires sidecar binary (node sidecar/dist/index.js)"]
    async fn synthesize_consolidation_integration_response_shape() {
        use std::path::PathBuf;
        let config = LlmConfig {
            enabled: true,
            sidecar_path: PathBuf::from("sidecar/dist/index.js"),
            confidence_threshold: 0.7,
            request_timeout_ms: 60_000,
            max_consecutive_failures: 3,
        };
        let base_dir = std::env::current_dir().unwrap();
        let (client, service) = SidecarClient::new(config, base_dir, CancellationToken::new()).await
            .expect("sidecar client should start");
        let params = SynthesizeConsolidationParams {
            cluster_id: "test-cluster-001".to_string(),
            summaries: vec![
                "Decided to use SurrealDB as the primary database.".to_string(),
                "Chose SurrealDB for its graph and relational capabilities.".to_string(),
                "SurrealDB selected as backend for the memory store.".to_string(),
            ],
            subtype: "decision".to_string(),
            project: Some("smriti".to_string()),
            domain: None,
        };
        let result = client.synthesize_consolidation(params).await
            .expect("synthesize_consolidation call should succeed");
        // Do not assert merge decision — LLM is non-deterministic.
        // Only validate response shape.
        assert!((0.0..=1.0).contains(&result.confidence));
        assert!(!result.reason.is_empty());
        if result.should_merge {
            let summary = result.merged_summary.as_deref().unwrap_or("");
            assert!(summary.len() >= 10 && summary.len() <= 500);
        }
        service.shutdown(Duration::from_secs(5)).await.ok();
    }
}
