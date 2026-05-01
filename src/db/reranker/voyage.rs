//! Voyage AI cross-encoder reranker backend.
//!
//! Opt-in stage 3.5 of the retrieval pipeline. Requires Voyage API key.
//! Mirrors structure of `src/db/embeddings/voyage.rs`.
//!
//! Privacy invariant: memory content is NEVER logged anywhere in this module.

use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::config::VoyageRerankerConfig;
use crate::db::reranker::{truncate_utf8, RerankCandidate};
use crate::error::{KbError, Result};
use crate::types::MemorySummary;

/// Maximum retry attempts on transient failures (429 / 5xx).
const MAX_RETRIES: u32 = 3;
/// Base backoff in milliseconds; doubled each retry (1s, 2s, 4s).
const BASE_BACKOFF_MS: u64 = 1000;
/// Uniform truncation limit for cross-encoder payload: content-or-summary, truncated here.
/// Keeps mixed chunk+parent result sets on comparable text lengths (ADR-020 §5).
const RERANK_TEXT_MAX_CHARS: usize = 3200;

/// Build the cross-encoder payload for a single candidate.
///
/// Uses `content` if present, falls back to `summary`. Truncated uniformly to
/// `RERANK_TEXT_MAX_CHARS` so that chunk rows (long content) and parent rows
/// (short summaries) are scored on comparable text lengths by the cross-encoder.
fn build_rerank_text(c: &RerankCandidate) -> &str {
    let body = c.content.as_deref().unwrap_or(c.summary.summary.as_str());
    truncate_utf8(body, RERANK_TEXT_MAX_CHARS)
}

/// Cross-encoder reranker backed by Voyage AI's `/v1/rerank` endpoint.
///
/// Clone is cheap — `Client` is internally reference-counted.
#[derive(Clone)]
pub struct VoyageReranker {
    client: Client,
    api_key: String,
    model: String,
    top_k_input: usize,
    base_url: String,
    max_retries: u32,
}

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct RerankRequest<'a> {
    query: &'a str,
    documents: &'a [String],
    model: &'a str,
    top_k: usize,
    return_documents: bool,
    truncation: bool,
}

#[derive(Deserialize)]
struct RerankResponse {
    data: Vec<RerankItem>,
}

#[derive(Deserialize)]
struct RerankItem {
    index: usize,
    #[allow(dead_code)]
    relevance_score: f64,
}

// ── Implementation ─────────────────────────────────────────────────────────────

impl VoyageReranker {
    /// Construct a `VoyageReranker`.
    ///
    /// Does NOT call the Voyage API at construction — same rationale as
    /// `VoyageEmbedder::new`: no startup ping, bad keys surface on first call.
    pub fn new(api_key: String, cfg: &VoyageRerankerConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| KbError::Embedding(format!("failed to build reqwest client for reranker: {e}")))?;

        tracing::info!(
            model = %cfg.model,
            top_k_input = cfg.top_k_input,
            "Voyage AI reranker ready (lazy auth — first rerank call surfaces invalid keys)"
        );

        Ok(Self {
            client,
            api_key,
            model: cfg.model.clone(),
            top_k_input: cfg.top_k_input,
            base_url: "https://api.voyageai.com/v1/rerank".to_string(),
            max_retries: MAX_RETRIES,
        })
    }

    /// Rerank `candidates` against `query`.
    ///
    /// Builds a uniform cross-encoder payload: `content` if present, else `summary`,
    /// truncated to 3200 chars. Returns `Vec<MemorySummary>` — content is dropped,
    /// wire shape unchanged. Soft-fail contract: returns input order on any error.
    /// Caller must not propagate the failure — per ADR-005 and ADR-017, recall
    /// degrades to RRF order on Voyage outage rather than returning an error.
    pub(crate) async fn rerank(&self, query: &str, mut candidates: Vec<RerankCandidate>) -> Vec<MemorySummary> {
        if candidates.len() <= 1 {
            return candidates.into_iter().map(|c| c.summary).collect();
        }

        // Truncate to configured top_k ceiling before sending.
        if candidates.len() > self.top_k_input {
            candidates.truncate(self.top_k_input);
        }

        let documents: Vec<String> = candidates
            .iter()
            .map(|c| build_rerank_text(c).to_string())
            .collect();

        let summaries: Vec<MemorySummary> = candidates.into_iter().map(|c| c.summary).collect();

        match self.call_api(query, &documents).await {
            Ok(indices) => {
                let mut reordered = Vec::with_capacity(indices.len());
                for idx in indices {
                    if idx < summaries.len() {
                        reordered.push(summaries[idx].clone());
                    }
                }
                if reordered.is_empty() {
                    summaries
                } else {
                    reordered
                }
            }
            Err(e) => {
                warn!(
                    candidate_count = summaries.len(),
                    "rerank failed, falling back to RRF order: {e}"
                );
                summaries
            }
        }
    }

    /// Call the Voyage rerank API with retry on transient errors.
    ///
    /// Returns indices in descending relevance order (as provided by the API).
    /// 4xx errors (except 429) fail immediately.
    /// Privacy: document text is never logged.
    async fn call_api(&self, query: &str, documents: &[String]) -> Result<Vec<usize>> {
        let body = RerankRequest {
            query,
            documents,
            model: &self.model,
            top_k: documents.len(),
            return_documents: false,
            truncation: true,
        };

        let mut attempt = 0u32;
        loop {
            attempt += 1;

            let resp = self
                .client
                .post(&self.base_url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
                .map_err(|e| KbError::Embedding(format!("Voyage rerank request failed: {e}")))?;

            let status = resp.status();

            if status.is_success() {
                let parsed: RerankResponse = resp
                    .json()
                    .await
                    .map_err(|e| KbError::Embedding(format!("Voyage rerank response parse error: {e}")))?;
                return Ok(parsed.data.into_iter().map(|item| item.index).collect());
            }

            // 429 or 5xx: potentially retryable
            let retryable = status.as_u16() == 429 || status.is_server_error();

            if !retryable || attempt > self.max_retries {
                let code = status.as_u16();
                let body_text = resp.text().await.unwrap_or_default();
                return Err(KbError::Embedding(format!(
                    "Voyage rerank API error {code} (attempt {attempt}): {body_text}"
                )));
            }

            let wait_ms = retry_wait_ms(&resp, attempt);
            warn!(
                attempt,
                status = status.as_u16(),
                wait_ms,
                "Voyage reranker transient error — retrying"
            );
            tokio::time::sleep(Duration::from_millis(wait_ms)).await;
        }
    }
}

/// Compute retry wait from Retry-After header or exponential backoff + jitter.
fn retry_wait_ms(resp: &reqwest::Response, attempt: u32) -> u64 {
    if let Some(ra) = resp.headers().get("retry-after")
        && let Ok(s) = ra.to_str()
        && let Ok(secs) = s.parse::<u64>()
    {
        return secs * 1000;
    }
    let base = BASE_BACKOFF_MS * (1u64 << (attempt - 1).min(6));
    let jitter = base / 4;
    base + (pseudo_jitter() % jitter.max(1))
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
    use crate::db::reranker::RerankCandidate;
    use crate::types::{Importance, MemoryId, MemorySubtype, MemoryType};

    fn make_summary(id: &str, summary: &str) -> MemorySummary {
        MemorySummary {
            id: MemoryId::from(id.to_string()),
            summary: summary.to_string(),
            memory_type: MemoryType::Semantic,
            subtype: MemorySubtype::Fact,
            importance: Importance::new(3).expect("valid importance"),
            project: None,
            domain: vec![],
            event_at: chrono::Utc::now(),
            tags: vec![],
        }
    }

    fn make_candidate(id: &str, summary: &str, content: Option<&str>) -> RerankCandidate {
        RerankCandidate {
            summary: make_summary(id, summary),
            content: content.map(String::from),
        }
    }

    fn make_reranker_with_url(base_url: &str, top_k_input: usize) -> VoyageReranker {
        let cfg = VoyageRerankerConfig {
            enabled: true,
            model: "rerank-2.5-lite".to_string(),
            top_k_input,
            policy: crate::types::RerankerPolicy::Adaptive,
        };
        let mut r = VoyageReranker::new("test-key".to_string(), &cfg).expect("build reranker");
        r.base_url = base_url.to_string();
        r.max_retries = 0;
        r
    }

    async fn start_mock_server(responses: Vec<(u16, String)>) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");

        tokio::spawn(async move {
            for (status_code, body) in responses {
                let Ok((mut stream, _)) = listener.accept().await else { break };
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf).await;

                let status_line = match status_code {
                    200 => "200 OK",
                    429 => "429 Too Many Requests",
                    500 => "500 Internal Server Error",
                    _ => "400 Bad Request",
                };
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            }
        });

        addr
    }

    /// Start a mock server that captures the raw request body and returns a canned response.
    async fn start_capturing_mock(
        response_body: String,
        captured: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = vec![0u8; 65536];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let raw = std::str::from_utf8(&buf[..n]).unwrap_or("").to_string();
                *captured.lock().unwrap() = raw;

                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(), response_body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn voyage_reranker_reorders_by_relevance() {
        let body = r#"{"object":"list","data":[{"index":2,"relevance_score":0.9},{"index":0,"relevance_score":0.7}],"usage":{"total_tokens":100}}"#;
        let addr = start_mock_server(vec![(200, body.to_string())]).await;
        let url = format!("http://{addr}");

        let reranker = make_reranker_with_url(&url, 50);

        let candidates = vec![
            make_candidate("doc0", "alpha", None),
            make_candidate("doc1", "beta", None),
            make_candidate("doc2", "gamma", None),
        ];

        let result = reranker.rerank("test query", candidates).await;

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id.as_str(), "doc2");
        assert_eq!(result[1].id.as_str(), "doc0");
    }

    #[tokio::test]
    async fn voyage_reranker_soft_fails_on_5xx() {
        let body = r#"{"error":"internal server error"}"#;
        let addr = start_mock_server(vec![(500, body.to_string())]).await;
        let url = format!("http://{addr}");

        let reranker = make_reranker_with_url(&url, 50);

        let candidates = vec![
            make_candidate("a", "first", None),
            make_candidate("b", "second", None),
            make_candidate("c", "third", None),
        ];
        let original_ids: Vec<String> = candidates.iter().map(|c| c.summary.id.as_str().to_string()).collect();

        let result = reranker.rerank("query", candidates).await;

        let result_ids: Vec<String> = result.iter().map(|m| m.id.as_str().to_string()).collect();
        assert_eq!(result_ids, original_ids);
    }

    #[tokio::test]
    async fn voyage_reranker_empty_input_no_api_call() {
        let reranker = make_reranker_with_url("http://127.0.0.1:1", 50);
        let result = reranker.rerank("query", vec![]).await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn voyage_reranker_truncates_to_top_k_input() {
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let received_doc_count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let received_clone = received_doc_count.clone();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");

        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = vec![0u8; 16384];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let raw = std::str::from_utf8(&buf[..n]).unwrap_or("");

                if let Some(body_start) = raw.find("\r\n\r\n") {
                    let json_body = &raw[body_start + 4..];
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_body)
                        && let Some(docs) = v["documents"].as_array()
                    {
                        *received_clone.lock().unwrap() = docs.len();
                    }
                }

                let body = r#"{"object":"list","data":[],"usage":{"total_tokens":10}}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            }
        });

        let url = format!("http://{addr}");
        let reranker = make_reranker_with_url(&url, 20);

        let candidates: Vec<RerankCandidate> = (0..25)
            .map(|i| make_candidate(&format!("doc{i}"), &format!("summary {i}"), None))
            .collect();

        let _ = reranker.rerank("query", candidates).await;

        assert_eq!(*received_doc_count.lock().unwrap(), 20);
    }

    #[tokio::test]
    async fn rerank_uses_content_when_present() {
        // Capture what the mock server receives; verify the content text was sent.
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let response_body = r#"{"object":"list","data":[{"index":0,"relevance_score":0.9},{"index":1,"relevance_score":0.5}],"usage":{"total_tokens":10}}"#.to_string();
        let addr = start_capturing_mock(response_body, captured.clone()).await;
        let url = format!("http://{addr}");
        let reranker = make_reranker_with_url(&url, 50);

        let content_text = "This is the full chunk content for the cross-encoder.";
        let candidates = vec![
            make_candidate("chunk1", "[chunk 1/3] parent summary here", Some(content_text)),
            make_candidate("parent1", "parent summary text", None),
        ];

        let _ = reranker.rerank("query", candidates).await;

        let raw = captured.lock().unwrap().clone();
        // The JSON request body should contain the content text, not the summary label.
        if let Some(body_start) = raw.find("\r\n\r\n") {
            let json_body = &raw[body_start + 4..];
            assert!(
                json_body.contains(content_text),
                "request body should contain content text; body={json_body:.200?}"
            );
            assert!(
                !json_body.contains("[chunk 1/3]"),
                "request body must not contain the chunk summary label"
            );
        }
    }

    #[tokio::test]
    async fn rerank_falls_back_to_summary_when_content_none() {
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let response_body = r#"{"object":"list","data":[{"index":0,"relevance_score":0.9},{"index":1,"relevance_score":0.5}],"usage":{"total_tokens":10}}"#.to_string();
        let addr = start_capturing_mock(response_body, captured.clone()).await;
        let url = format!("http://{addr}");
        let reranker = make_reranker_with_url(&url, 50);

        let summary_text = "This is the summary used as fallback.";
        let candidates = vec![
            make_candidate("mem1", summary_text, None), // no content
            make_candidate("mem2", "another summary", None),
        ];

        let _ = reranker.rerank("query", candidates).await;

        let raw = captured.lock().unwrap().clone();
        if let Some(body_start) = raw.find("\r\n\r\n") {
            let json_body = &raw[body_start + 4..];
            assert!(
                json_body.contains(summary_text),
                "request body should contain summary text when content is None; body={json_body:.200?}"
            );
        }
    }
}
