//! Voyage AI cloud embedding backend.
//!
//! Opt-in alternative to local fastembed. Requires `VOYAGE_API_KEY` or
//! `embeddings.voyage.api_key` in config. Default stays local — this module
//! is only constructed when `embeddings.provider = "voyage"`.
//!
//! ADR-020 unification: `voyage-context-3` exists ONLY on
//! `/v1/contextualizedembeddings`. Both the single-input paths
//! (`embed_query`, `embed_document`) and the batched chunk path
//! (`embed_document_chunks`) route through that endpoint. The legacy
//! `/v1/embeddings` endpoint is not used anywhere in this backend.
//!
//! Privacy invariant: input text is NEVER logged anywhere in this module.

pub mod contextualized;

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use tracing::{info, warn};

use crate::db::embeddings::fingerprint::EmbedderFingerprint;
use crate::db::embeddings::traits::Embedder;
use crate::error::{KbError, Result};

use contextualized::{
    ContextualizedEmbedRequest, ContextualizedEmbedResponse, CONTEXTUALIZED_ENDPOINT,
};

/// Maximum retry attempts on transient failures (429 / 5xx).
const MAX_RETRIES: u32 = 3;
/// Base backoff in milliseconds; doubled each retry (1s, 2s, 4s).
const BASE_BACKOFF_MS: u64 = 1000;

/// Cloud embedder backed by Voyage AI's `/v1/contextualizedembeddings` endpoint.
///
/// Clone is cheap — inner state is reference-counted.
#[derive(Clone)]
pub struct VoyageEmbedder {
    client: Client,
    api_key: String,
    model: String,
    dimensions: usize,
    /// Overridable base URL for tests. `None` → use production constants.
    base_url_override: Option<String>,
}

// ── Implementation ─────────────────────────────────────────────────────────────

impl VoyageEmbedder {
    /// Construct a `VoyageEmbedder`.
    ///
    /// Does NOT call the Voyage API at startup — burning even one token per
    /// process boot wastes quota under crash-loops, blocks server init by
    /// 200-800ms, and fails behind enterprise HTTPS proxies in confusing
    /// ways. Bad API keys surface on the first real embed call with a clear
    /// 401 error, which is good enough.
    pub fn new(api_key: String, model: String, dimensions: usize, timeout_ms: u64) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|e| KbError::Embedding(format!("failed to build reqwest client: {e}")))?;

        let embedder = Self { client, api_key, model, dimensions, base_url_override: None };

        info!(
            model = %embedder.model,
            dimensions = embedder.dimensions,
            "Voyage AI embedder ready (lazy auth — first embed surfaces invalid keys)"
        );

        Ok(embedder)
    }

    pub fn model_name(&self) -> &str {
        &self.model
    }

    /// Embed a batch of independent documents.
    ///
    /// Each text is sent as its own single-chunk "document" via the
    /// contextualized endpoint (`inputs: [[t1], [t2], ...]`). Voyage caps
    /// the outer list at 1000 documents per request.
    pub async fn embed_documents_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let owned: Vec<Vec<String>> = texts.iter().map(|t| vec![(*t).to_string()]).collect();
        let docs = self.contextualized_call(&owned, "document").await?;
        // Each doc has exactly one chunk vector.
        docs.into_iter()
            .map(|chunks| {
                chunks
                    .into_iter()
                    .next()
                    .ok_or_else(|| KbError::Embedding(
                        "Voyage contextualized returned doc with empty chunks".to_string(),
                    ))
            })
            .collect()
    }

    /// Embed chunks of a SINGLE document.
    ///
    /// The Voyage contextualized endpoint performs cross-chunk attention so
    /// each chunk embedding is informed by the full document context. Returns
    /// one vector per chunk in input order.
    ///
    /// `input_type` should be `"document"` or `"query"`.
    pub async fn contextualized_embed(
        &self,
        doc_chunks: Vec<String>,
        input_type: &str,
    ) -> Result<Vec<Vec<f32>>> {
        let inputs = vec![doc_chunks];
        let mut docs = self.contextualized_call(&inputs, input_type).await?;
        docs.pop().ok_or_else(|| {
            KbError::Embedding("Voyage contextualized returned empty data array".to_string())
        })
    }

    // ── Internal HTTP + retry ────────────────────────────────────────────────

    /// Single source of truth for Voyage HTTP: posts to
    /// `/v1/contextualizedembeddings` with retry on 429/5xx.
    ///
    /// Caller provides the full `inputs: list-of-lists` shape. Returns one
    /// `Vec<Vec<f32>>` per outer input (i.e. per document); inner vec is the
    /// chunk vectors of that document in order.
    ///
    /// 4xx (other than 429) fails immediately — no retry.
    /// Privacy: input text is never logged.
    async fn contextualized_call(
        &self,
        inputs: &[Vec<String>],
        input_type: &str,
    ) -> Result<Vec<Vec<Vec<f32>>>> {
        // Borrow into &[&str] structure for serialization.
        let borrowed: Vec<Vec<&str>> = inputs
            .iter()
            .map(|doc| doc.iter().map(|s| s.as_str()).collect())
            .collect();
        let body = ContextualizedEmbedRequest {
            inputs: borrowed,
            model: &self.model,
            input_type,
            output_dimension: Some(self.dimensions),
        };

        let endpoint = match &self.base_url_override {
            Some(base) => format!("{}/v1/contextualizedembeddings", base.trim_end_matches('/')),
            None => CONTEXTUALIZED_ENDPOINT.to_string(),
        };

        let mut attempt = 0u32;
        loop {
            attempt += 1;

            let resp = self
                .client
                .post(&endpoint)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
                .map_err(|e| {
                    KbError::Embedding(format!("Voyage contextualized request failed: {e}"))
                })?;

            let status = resp.status();

            if status.is_success() {
                let parsed: ContextualizedEmbedResponse = resp.json().await.map_err(|e| {
                    KbError::Embedding(format!("Voyage contextualized response parse error: {e}"))
                })?;

                let docs: Vec<Vec<Vec<f32>>> = parsed
                    .data
                    .into_iter()
                    .map(|doc| doc.data.into_iter().map(|c| c.embedding).collect())
                    .collect();
                return Ok(docs);
            }

            let retryable = status.as_u16() == 429 || status.is_server_error();

            if !retryable || attempt > MAX_RETRIES {
                let code = status.as_u16();
                let body_text = resp.text().await.unwrap_or_default();
                return Err(KbError::Embedding(format!(
                    "Voyage contextualized API error {code} (attempt {attempt}): {body_text}"
                )));
            }

            let wait_ms = retry_wait_ms(&resp, attempt);
            warn!(
                attempt,
                status = status.as_u16(),
                wait_ms,
                "Voyage contextualized transient error — retrying"
            );
            tokio::time::sleep(Duration::from_millis(wait_ms)).await;
        }
    }
}

#[async_trait]
impl Embedder for VoyageEmbedder {
    fn fingerprint(&self) -> EmbedderFingerprint {
        EmbedderFingerprint {
            provider: "voyage".to_string(),
            model: self.model.clone(),
            dimensions: self.dimensions,
        }
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn max_input_tokens(&self) -> usize {
        32000
    }

    fn max_chunks_per_doc(&self) -> usize {
        1000
    }

    fn recommended_chunk_tokens(&self) -> usize {
        800
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        // Single-input contextualized call: one doc with one chunk.
        let mut chunks = self
            .contextualized_embed(vec![text.to_string()], "query")
            .await?;
        chunks
            .pop()
            .ok_or_else(|| KbError::Embedding("Voyage returned empty data array".to_string()))
    }

    async fn embed_document(&self, text: &str) -> Result<Vec<f32>> {
        let mut chunks = self
            .contextualized_embed(vec![text.to_string()], "document")
            .await?;
        chunks
            .pop()
            .ok_or_else(|| KbError::Embedding("Voyage returned empty data array".to_string()))
    }

    /// Batch-embed all chunks of one document via the contextualized endpoint.
    ///
    /// Cross-chunk attention produces higher-quality chunk vectors than N
    /// independent embedding calls would.
    async fn embed_document_chunks(&self, chunks: &[String]) -> Result<Vec<Vec<f32>>> {
        self.contextualized_embed(chunks.to_vec(), "document").await
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
    // Exponential: 1s, 2s, 4s
    let base = BASE_BACKOFF_MS * (1u64 << (attempt - 1).min(6));
    // Add up to 25% jitter to avoid thundering herd.
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

    // ── Request body shape ────────────────────────────────────────────────────

    #[test]
    fn contextualized_request_serializes_correctly() {
        let inputs: Vec<Vec<&str>> = vec![vec!["hello world"]];
        let req = ContextualizedEmbedRequest {
            inputs,
            model: "voyage-context-3",
            input_type: "document",
            output_dimension: Some(1024),
        };
        let json = serde_json::to_value(&req).expect("serialize");
        assert_eq!(json["model"], "voyage-context-3");
        assert_eq!(json["input_type"], "document");
        assert_eq!(json["output_dimension"], 1024);
        assert_eq!(json["inputs"][0][0], "hello world");
    }

    #[test]
    fn contextualized_request_query_input_type() {
        let inputs: Vec<Vec<&str>> = vec![vec!["what is smriti"]];
        let req = ContextualizedEmbedRequest {
            inputs,
            model: "voyage-context-3",
            input_type: "query",
            output_dimension: Some(256),
        };
        let json = serde_json::to_value(&req).expect("serialize");
        assert_eq!(json["input_type"], "query");
        assert_eq!(json["output_dimension"], 256);
    }

    #[test]
    fn contextualized_response_deserializes_correctly() {
        let raw = r#"{"data":[{"data":[{"embedding":[0.1,0.2,0.3],"index":0}],"index":0}],"usage":{"total_tokens":5}}"#;
        let resp: ContextualizedEmbedResponse = serde_json::from_str(raw).expect("deserialize");
        assert_eq!(resp.data.len(), 1);
        assert_eq!(resp.data[0].data.len(), 1);
        assert!((resp.data[0].data[0].embedding[0] - 0.1f32).abs() < f32::EPSILON);
    }

    // ── Mock server tests ─────────────────────────────────────────────────────

    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Minimal HTTP/1.1 mock server built on raw TCP.
    /// Returns the provided `responses` in order (one per accepted connection).
    async fn start_mock_server(responses: Vec<(u16, String)>) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");

        tokio::spawn(async move {
            for (status_code, body) in responses {
                let Ok((mut stream, _)) = listener.accept().await else { break };
                // Drain the request headers (don't care about body for mock purposes).
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;

                let status_line = match status_code {
                    200 => "200 OK",
                    401 => "401 Unauthorized",
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

    /// Build a single-document, single-chunk contextualized response — the
    /// shape `embed_query` and `embed_document` see after unification.
    fn ok_body(dims: usize) -> String {
        contextualized_ok_body(1, dims)
    }

    fn build_embedder_for(addr: SocketAddr) -> VoyageEmbedder {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        VoyageEmbedder {
            client,
            api_key: "test-key".to_string(),
            model: "voyage-context-3".to_string(),
            dimensions: 4,
            base_url_override: Some(format!("http://{addr}")),
        }
    }

    /// Build a mock HTTP response body for the contextualized endpoint.
    ///
    /// Returns N chunks each with `dims`-dimensional vectors.
    fn contextualized_ok_body(chunk_count: usize, dims: usize) -> String {
        let chunks: Vec<String> = (0..chunk_count)
            .map(|ci| {
                let vec: Vec<f32> = (0..dims).map(|i| (ci * dims + i) as f32 * 0.001).collect();
                let arr: Vec<String> = vec.iter().map(|f| f.to_string()).collect();
                format!(
                    r#"{{"embedding":[{}],"index":{ci}}}"#,
                    arr.join(",")
                )
            })
            .collect();
        format!(
            r#"{{"data":[{{"data":[{}],"index":0}}],"usage":{{"total_tokens":{}}}}}"#,
            chunks.join(","),
            chunk_count * 5
        )
    }

    #[tokio::test]
    async fn voyage_happy_path_returns_vector() {
        let addr = start_mock_server(vec![(200, ok_body(4))]).await;
        let embedder = build_embedder_for(addr);
        let result = embedder.embed_document("hello world").await.expect("embed_document");
        assert_eq!(result.len(), 4);
    }

    /// Test that 401 (4xx non-429) produces immediate error, no retry.
    #[tokio::test]
    async fn voyage_401_fast_fails() {
        let body = r#"{"detail":"invalid api key"}"#.to_string();
        let addr = start_mock_server(vec![(401, body)]).await;
        let embedder = build_embedder_for(addr);
        let err = embedder.embed_document("test").await.unwrap_err();
        assert!(err.to_string().contains("401"), "expected 401 in error: {err}");
    }

    /// `embed_document_chunks` calls the contextualized endpoint and returns
    /// one vector per chunk with the correct dimensionality.
    #[tokio::test]
    async fn voyage_embed_document_chunks_happy_path() {
        let chunk_count = 3;
        let dims = 4;
        let addr = start_mock_server(vec![(200, contextualized_ok_body(chunk_count, dims))]).await;
        let embedder = build_embedder_for(addr);

        let chunks: Vec<String> = (0..chunk_count).map(|i| format!("chunk {i}")).collect();
        let vectors = embedder.embed_document_chunks(&chunks).await.expect("embed_document_chunks");

        assert_eq!(vectors.len(), chunk_count, "one vector per chunk");
        for (i, vec) in vectors.iter().enumerate() {
            assert_eq!(vec.len(), dims, "chunk {i} has wrong dimensionality");
        }
    }

    /// Test retry logic classification: 429 and 5xx are retryable.
    #[test]
    fn retry_classification_correct() {
        let retryable = |code: u16| code == 429 || (500..600).contains(&code);
        assert!(retryable(429));
        assert!(retryable(500));
        assert!(retryable(503));
        assert!(!retryable(401));
        assert!(!retryable(400));
        assert!(!retryable(404));
    }

    /// Test exponential backoff values.
    #[test]
    fn backoff_grows_exponentially() {
        // Without a real response we test the base calculation directly.
        // BASE_BACKOFF_MS * 2^(attempt-1): attempt=1 → 1000ms, attempt=2 → 2000ms
        let base_for = |attempt: u32| -> u64 {
            BASE_BACKOFF_MS * (1u64 << (attempt - 1).min(6))
        };
        assert_eq!(base_for(1), 1000);
        assert_eq!(base_for(2), 2000);
        assert_eq!(base_for(3), 4000);
    }
}
