//! Voyage AI Batch API embedder — bench-only.
//!
//! Uses Voyage's `/v1/batches` endpoint (33% discount, 12h SLA).
//! NOT suitable for production write path — use [`super::voyage::VoyageEmbedder`]
//! for latency-critical paths.
//!
//! Workflow:
//!   1. Upload JSONL request file via `/v1/files` (multipart).
//!   2. Submit batch job via `/v1/batches`.
//!   3. Poll `/v1/batches/{id}` until terminal state.
//!   4. Download result JSONL via `/v1/files/{output_file_id}/content`.
//!   5. Parse results, re-order by `custom_id` to match input order.
//!
//! Privacy invariant: input text is NEVER logged anywhere in this module.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::info;

/// Default interval between batch status polls.
const DEFAULT_POLL_INTERVAL_SECS: u64 = 30;
/// Maximum total wait: 14h (12h SLA + 2h buffer).
const MAX_WAIT_SECS: u64 = 14 * 3600;

// ── Public types ──────────────────────────────────────────────────────────────

/// Voyage Batch API client. Cheap to clone — inner `reqwest::Client` is `Arc`-backed.
pub struct VoyageBatchClient {
    client: reqwest::Client,
    api_key: String,
    model: String,
    output_dimension: usize,
    base_url: String,
    poll_interval_secs: u64,
}

/// Handle returned by [`VoyageBatchClient::submit_batch`].
///
/// Persist this between `submit_batch` and `fetch_results` if crash-safe
/// restart is needed — re-submitting on crash wastes quota.
pub struct BatchHandle {
    pub file_id: String,
    pub batch_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum VoyageBatchError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Voyage API error {status}: {message}")]
    Api { status: u16, message: String },
    #[error("Batch failed: {reason}")]
    BatchFailed { reason: String },
    #[error("Result count mismatch: expected {expected}, got {got}")]
    CountMismatch { expected: usize, got: usize },
    #[error("JSONL parse error at line {line}: {message}")]
    Parse { line: usize, message: String },
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ── Wire types (request) ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct BatchRequest<'a> {
    custom_id: String,
    body: BatchRequestBody<'a>,
}

#[derive(Serialize)]
struct BatchRequestBody<'a> {
    input: [&'a str; 1],
}

#[derive(Serialize)]
struct SubmitBatchRequest<'a> {
    input_file_id: &'a str,
    endpoint: &'static str,
    completion_window: &'static str,
    request_params: BatchRequestParams<'a>,
}

#[derive(Serialize)]
struct BatchRequestParams<'a> {
    model: &'a str,
    input_type: &'a str,
    output_dimension: usize,
}

// ── Wire types (response) ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct FileUploadResponse {
    id: String,
}

#[derive(Deserialize)]
struct BatchSubmitResponse {
    id: String,
}

#[derive(Deserialize)]
struct BatchStatusResponse {
    status: String,
    output_file_id: Option<String>,
    errors: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct BatchResultLine {
    custom_id: String,
    response: Option<BatchResponse>,
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct BatchResponse {
    status_code: u16,
    body: serde_json::Value,
}

// ── Implementation ────────────────────────────────────────────────────────────

impl VoyageBatchClient {
    pub fn new(
        api_key: String,
        model: String,
        output_dimension: usize,
    ) -> Result<Self, VoyageBatchError> {
        Self::with_options(
            api_key,
            model,
            output_dimension,
            "https://api.voyageai.com",
            DEFAULT_POLL_INTERVAL_SECS,
        )
    }

    /// Constructor with overridable base URL and poll interval — used in tests.
    pub fn with_options(
        api_key: String,
        model: String,
        output_dimension: usize,
        base_url: &str,
        poll_interval_secs: u64,
    ) -> Result<Self, VoyageBatchError> {
        let client = reqwest::Client::builder()
            // Batch operations have long SLAs; no per-request timeout.
            .build()
            .map_err(VoyageBatchError::Http)?;
        Ok(Self {
            client,
            api_key,
            model,
            output_dimension,
            base_url: base_url.trim_end_matches('/').to_string(),
            poll_interval_secs,
        })
    }

    /// Upload JSONL file + submit batch. Returns a handle for polling.
    ///
    /// `input_type` should be `"document"` or `"query"`.
    pub async fn submit_batch(
        &self,
        documents: &[String],
        input_type: &str,
    ) -> Result<BatchHandle, VoyageBatchError> {
        let jsonl_bytes = self.build_jsonl(documents, input_type)?;
        let byte_count = jsonl_bytes.len();
        let file_id = self.upload_file(jsonl_bytes).await?;
        info!(file_id, byte_count, "voyage batch file uploaded");

        let batch_id = self.create_batch(&file_id, input_type).await?;
        info!(batch_id, doc_count = documents.len(), "voyage batch submitted");

        Ok(BatchHandle { file_id, batch_id })
    }

    /// Poll until the batch reaches a terminal state (`completed`, `failed`, `expired`).
    /// Returns the `output_file_id` on success.
    ///
    /// Caps total wait at 14h; returns `BatchFailed { reason: "timeout" }` if exceeded.
    pub async fn await_completion(
        &self,
        handle: &BatchHandle,
    ) -> Result<String, VoyageBatchError> {
        let mut elapsed_secs = 0u64;
        let batch_id = &handle.batch_id;

        loop {
            let status = self.poll_batch(batch_id).await?;
            info!(batch_id, status = %status.status, "voyage batch status");

            match status.status.as_str() {
                "completed" => {
                    return status.output_file_id.ok_or_else(|| VoyageBatchError::BatchFailed {
                        reason: "completed but output_file_id missing".to_string(),
                    });
                }
                "failed" | "expired" | "cancelled" => {
                    let reason = format!(
                        "batch {} with status={}; errors={:?}",
                        batch_id,
                        status.status,
                        status.errors
                    );
                    return Err(VoyageBatchError::BatchFailed { reason });
                }
                // "validating" | "in_progress" | unknown — keep polling
                _ => {}
            }

            if elapsed_secs >= MAX_WAIT_SECS {
                return Err(VoyageBatchError::BatchFailed {
                    reason: "timeout".to_string(),
                });
            }

            let wait = jittered_wait(self.poll_interval_secs);
            tokio::time::sleep(Duration::from_secs(wait)).await;
            elapsed_secs = elapsed_secs.saturating_add(wait);
        }
    }

    /// Download result file, parse JSONL, return embeddings in input order.
    ///
    /// Results are re-ordered by `custom_id` (`req-{index}`), so out-of-order
    /// Voyage responses are handled correctly.
    pub async fn fetch_results(
        &self,
        output_file_id: &str,
        expected_count: usize,
    ) -> Result<Vec<Vec<f32>>, VoyageBatchError> {
        let content = self.download_file(output_file_id).await?;
        parse_result_jsonl(&content, expected_count)
    }

    /// Convenience: submit + await + fetch in one call.
    ///
    /// For crash-safe restart, use `submit_batch` + `await_completion` +
    /// `fetch_results` separately and persist the `BatchHandle`.
    pub async fn embed_documents_batch(
        &self,
        documents: &[String],
    ) -> Result<Vec<Vec<f32>>, VoyageBatchError> {
        let handle = self.submit_batch(documents, "document").await?;
        let out_file = self.await_completion(&handle).await?;
        self.fetch_results(&out_file, documents.len()).await
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn build_jsonl(
        &self,
        documents: &[String],
        _input_type: &str,
    ) -> Result<Vec<u8>, VoyageBatchError> {
        let mut out = Vec::new();
        for (i, doc) in documents.iter().enumerate() {
            let req = BatchRequest {
                custom_id: format!("req-{i}"),
                body: BatchRequestBody {
                    input: [doc.as_str()],
                },
            };
            let line = serde_json::to_string(&req)?;
            out.extend_from_slice(line.as_bytes());
            out.push(b'\n');
        }
        Ok(out)
    }

    async fn upload_file(&self, jsonl_bytes: Vec<u8>) -> Result<String, VoyageBatchError> {
        let part = reqwest::multipart::Part::bytes(jsonl_bytes)
            .file_name("requests.jsonl")
            .mime_str("application/jsonl")
            .map_err(VoyageBatchError::Http)?;
        let form = reqwest::multipart::Form::new()
            .text("purpose", "batch")
            .part("file", part);

        let resp = self
            .client
            .post(format!("{}/v1/files", self.base_url))
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let message = resp.text().await.unwrap_or_default();
            return Err(VoyageBatchError::Api { status, message });
        }

        let parsed: FileUploadResponse = resp.json().await?;
        Ok(parsed.id)
    }

    async fn create_batch(&self, file_id: &str, input_type: &str) -> Result<String, VoyageBatchError> {
        let body = SubmitBatchRequest {
            input_file_id: file_id,
            endpoint: "/v1/embeddings",
            completion_window: "12h",
            request_params: BatchRequestParams {
                model: &self.model,
                input_type,
                output_dimension: self.output_dimension,
            },
        };

        let resp = self
            .client
            .post(format!("{}/v1/batches", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let message = resp.text().await.unwrap_or_default();
            return Err(VoyageBatchError::Api { status, message });
        }

        let parsed: BatchSubmitResponse = resp.json().await?;
        Ok(parsed.id)
    }

    async fn poll_batch(&self, batch_id: &str) -> Result<BatchStatusResponse, VoyageBatchError> {
        let resp = self
            .client
            .get(format!("{}/v1/batches/{batch_id}", self.base_url))
            .bearer_auth(&self.api_key)
            .send()
            .await?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let message = resp.text().await.unwrap_or_default();
            return Err(VoyageBatchError::Api { status, message });
        }

        Ok(resp.json().await?)
    }

    async fn download_file(&self, file_id: &str) -> Result<String, VoyageBatchError> {
        let resp = self
            .client
            .get(format!("{}/v1/files/{file_id}/content", self.base_url))
            .bearer_auth(&self.api_key)
            .send()
            .await?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let message = resp.text().await.unwrap_or_default();
            return Err(VoyageBatchError::Api { status, message });
        }

        Ok(resp.text().await?)
    }
}

/// Parse batch result JSONL into embeddings ordered by input index.
fn parse_result_jsonl(
    content: &str,
    expected_count: usize,
) -> Result<Vec<Vec<f32>>, VoyageBatchError> {
    let mut map: HashMap<String, Vec<f32>> = HashMap::with_capacity(expected_count);

    for (line_no, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        let result: BatchResultLine =
            serde_json::from_str(line).map_err(|e| VoyageBatchError::Parse {
                line: line_no + 1,
                message: e.to_string(),
            })?;

        if result.error.is_some() {
            return Err(VoyageBatchError::BatchFailed {
                reason: format!(
                    "line {}: per-request error for custom_id={}",
                    line_no + 1,
                    result.custom_id
                ),
            });
        }

        let resp = result
            .response
            .ok_or_else(|| VoyageBatchError::Parse {
                line: line_no + 1,
                message: format!("missing response for custom_id={}", result.custom_id),
            })?;

        if resp.status_code != 200 {
            return Err(VoyageBatchError::BatchFailed {
                reason: format!(
                    "line {}: status_code={} for custom_id={}",
                    line_no + 1,
                    resp.status_code,
                    result.custom_id
                ),
            });
        }

        let embedding: Vec<f32> =
            serde_json::from_value(resp.body["data"][0]["embedding"].clone()).map_err(|e| {
                VoyageBatchError::Parse {
                    line: line_no + 1,
                    message: format!("failed to parse embedding: {e}"),
                }
            })?;

        map.insert(result.custom_id, embedding);
    }

    // Re-assemble in input order via req-{index} keys.
    let mut output = Vec::with_capacity(expected_count);
    for i in 0..expected_count {
        let key = format!("req-{i}");
        match map.remove(&key) {
            Some(emb) => output.push(emb),
            None => {
                return Err(VoyageBatchError::CountMismatch {
                    expected: expected_count,
                    got: map.len() + output.len(),
                });
            }
        }
    }

    Ok(output)
}

/// Compute a jittered wait from a base interval (±20%).
///
/// Avoids thundering herd when multiple bench processes poll simultaneously.
fn jittered_wait(base_secs: u64) -> u64 {
    if base_secs == 0 {
        return 0;
    }
    let jitter_range = base_secs / 5; // 20% of base
    let jitter = pseudo_jitter() % jitter_range.max(1);
    // Randomly add or subtract jitter based on parity.
    if pseudo_jitter().is_multiple_of(2) {
        base_secs.saturating_add(jitter)
    } else {
        base_secs.saturating_sub(jitter)
    }
}

/// Deterministic pseudo-jitter from current time — avoids pulling in `rand`.
fn pseudo_jitter() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_micros() as u64)
        .unwrap_or(0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Unit tests (no HTTP) ──────────────────────────────────────────────────

    #[test]
    fn build_jsonl_produces_correct_lines() {
        let client = VoyageBatchClient::with_options(
            "key".to_string(),
            "voyage-4-large".to_string(),
            1024,
            "http://localhost",
            0,
        )
        .expect("reqwest client init");
        let docs = vec!["hello".to_string(), "world".to_string()];
        let bytes = client.build_jsonl(&docs, "document").unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);

        let l0: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(l0["custom_id"], "req-0");
        assert_eq!(l0["body"]["input"][0], "hello");
        // Per Voyage batch spec, model/input_type/output_dimension live at
        // batch-level request_params (see SubmitBatchRequest); per-line body
        // must contain ONLY `input` — extra fields are rejected with 422.
        assert!(l0.get("method").is_none());
        assert!(l0.get("url").is_none());
        assert!(l0["body"].get("model").is_none());
        assert!(l0["body"].get("input_type").is_none());
        assert!(l0["body"].get("output_dimension").is_none());

        let l1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(l1["custom_id"], "req-1");
        assert_eq!(l1["body"]["input"][0], "world");
    }

    #[test]
    fn parse_result_jsonl_reorders_by_custom_id() {
        // Simulate out-of-order result: req-2, req-0, req-1
        let jsonl = concat!(
            r#"{"custom_id":"req-2","response":{"status_code":200,"body":{"data":[{"embedding":[0.2,0.2],"index":2}]}},"error":null}"#, "\n",
            r#"{"custom_id":"req-0","response":{"status_code":200,"body":{"data":[{"embedding":[0.0,0.0],"index":0}]}},"error":null}"#, "\n",
            r#"{"custom_id":"req-1","response":{"status_code":200,"body":{"data":[{"embedding":[0.1,0.1],"index":1}]}},"error":null}"#, "\n",
        );
        let result = parse_result_jsonl(jsonl, 3).unwrap();
        assert_eq!(result.len(), 3);
        assert!((result[0][0] - 0.0f32).abs() < f32::EPSILON, "index 0 first");
        assert!((result[1][0] - 0.1f32).abs() < f32::EPSILON, "index 1 second");
        assert!((result[2][0] - 0.2f32).abs() < f32::EPSILON, "index 2 third");
    }

    #[test]
    fn parse_result_jsonl_count_mismatch() {
        // 2 results when 3 expected
        let jsonl = concat!(
            r#"{"custom_id":"req-0","response":{"status_code":200,"body":{"data":[{"embedding":[0.0],"index":0}]}},"error":null}"#, "\n",
            r#"{"custom_id":"req-1","response":{"status_code":200,"body":{"data":[{"embedding":[0.1],"index":1}]}},"error":null}"#, "\n",
        );
        let err = parse_result_jsonl(jsonl, 3).unwrap_err();
        assert!(
            matches!(err, VoyageBatchError::CountMismatch { expected: 3, .. }),
            "expected CountMismatch, got: {err}"
        );
    }

    #[test]
    fn parse_result_jsonl_per_line_error_propagates() {
        let jsonl = concat!(
            r#"{"custom_id":"req-0","response":null,"error":{"message":"quota exceeded"}}"#,
            "\n",
        );
        let err = parse_result_jsonl(jsonl, 1).unwrap_err();
        assert!(
            matches!(err, VoyageBatchError::BatchFailed { .. }),
            "expected BatchFailed, got: {err}"
        );
    }

    #[test]
    fn parse_result_jsonl_non200_status_propagates() {
        let jsonl = concat!(
            r#"{"custom_id":"req-0","response":{"status_code":429,"body":{}},"error":null}"#,
            "\n",
        );
        let err = parse_result_jsonl(jsonl, 1).unwrap_err();
        assert!(
            matches!(err, VoyageBatchError::BatchFailed { .. }),
            "expected BatchFailed, got: {err}"
        );
    }

    // ── HTTP integration tests (wiremock) ────────────────────────────────────

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_client(base_url: &str) -> VoyageBatchClient {
        VoyageBatchClient::with_options(
            "test-api-key".to_string(),
            "voyage-4-large".to_string(),
            1024,
            base_url,
            0, // instant poll for tests
        )
        .expect("reqwest client init")
    }

    fn file_upload_response(file_id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": file_id,
            "object": "file",
            "bytes": 1234,
            "created_at": 1700000000_u64,
            "filename": "requests.jsonl",
            "purpose": "batch"
        })
    }

    fn batch_submit_response(batch_id: &str, file_id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": batch_id,
            "object": "batch",
            "status": "validating",
            "input_file_id": file_id
        })
    }

    fn batch_status_response(batch_id: &str, status: &str, output_file_id: Option<&str>) -> serde_json::Value {
        let mut v = serde_json::json!({
            "id": batch_id,
            "object": "batch",
            "status": status,
            "input_file_id": "file-abc"
        });
        if let Some(oid) = output_file_id {
            v["output_file_id"] = serde_json::Value::String(oid.to_string());
        }
        v
    }

    fn result_jsonl_two_docs() -> String {
        concat!(
            r#"{"custom_id":"req-1","response":{"status_code":200,"body":{"data":[{"embedding":[0.3,0.4],"index":1}],"usage":{"total_tokens":5}}},"error":null}"#, "\n",
            r#"{"custom_id":"req-0","response":{"status_code":200,"body":{"data":[{"embedding":[0.1,0.2],"index":0}],"usage":{"total_tokens":5}}},"error":null}"#, "\n",
        ).to_string()
    }

    #[tokio::test]
    async fn voyage_batch_submit_uploads_jsonl_then_creates_batch() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(file_upload_response("file-abc123")))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/v1/batches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(batch_submit_response("batch-xyz789", "file-abc123")))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let docs = vec!["doc one".to_string(), "doc two".to_string()];
        let handle = client.submit_batch(&docs, "document").await.unwrap();

        assert_eq!(handle.file_id, "file-abc123");
        assert_eq!(handle.batch_id, "batch-xyz789");

        // wiremock verifies expect(1) on drop
    }

    #[tokio::test]
    async fn voyage_batch_await_polls_until_completed() {
        let server = MockServer::start().await;

        // Stateful polling: validating → in_progress → completed.
        // wiremock's up_to_n_times(1) drains each mock in registration order.
        let responses = [
            batch_status_response("batch-1", "validating", None),
            batch_status_response("batch-1", "in_progress", None),
            batch_status_response("batch-1", "completed", Some("out-file-1")),
        ];

        for resp in &responses {
            Mock::given(method("GET"))
                .and(path("/v1/batches/batch-1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(resp.clone()))
                .up_to_n_times(1)
                .mount(&server)
                .await;
        }

        let client = make_client(&server.uri());
        let handle = BatchHandle {
            file_id: "file-abc".to_string(),
            batch_id: "batch-1".to_string(),
        };
        let output_file_id = client.await_completion(&handle).await.unwrap();
        assert_eq!(output_file_id, "out-file-1");
    }

    #[tokio::test]
    async fn voyage_batch_fetch_returns_in_input_order() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/files/out-file-2/content"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(result_jsonl_two_docs()),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let result = client.fetch_results("out-file-2", 2).await.unwrap();

        assert_eq!(result.len(), 2);
        // req-0 should be first despite being second in the JSONL
        assert!((result[0][0] - 0.1f32).abs() < f32::EPSILON, "req-0 first");
        assert!((result[1][0] - 0.3f32).abs() < f32::EPSILON, "req-1 second");
    }

    #[tokio::test]
    async fn voyage_batch_fetch_count_mismatch() {
        let server = MockServer::start().await;

        // Only 2 results but we expect 3
        Mock::given(method("GET"))
            .and(path("/v1/files/out-file-3/content"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(result_jsonl_two_docs()),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let err = client.fetch_results("out-file-3", 3).await.unwrap_err();
        assert!(
            matches!(err, VoyageBatchError::CountMismatch { expected: 3, .. }),
            "expected CountMismatch, got: {err}"
        );
    }

    #[tokio::test]
    async fn voyage_batch_failed_status_propagates() {
        let server = MockServer::start().await;

        let failed_resp = serde_json::json!({
            "id": "batch-fail",
            "object": "batch",
            "status": "failed",
            "input_file_id": "file-fail",
            "errors": {"message": "internal error"}
        });

        Mock::given(method("GET"))
            .and(path("/v1/batches/batch-fail"))
            .respond_with(ResponseTemplate::new(200).set_body_json(failed_resp))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let handle = BatchHandle {
            file_id: "file-fail".to_string(),
            batch_id: "batch-fail".to_string(),
        };
        let err = client.await_completion(&handle).await.unwrap_err();
        assert!(
            matches!(err, VoyageBatchError::BatchFailed { .. }),
            "expected BatchFailed, got: {err}"
        );
    }
}
