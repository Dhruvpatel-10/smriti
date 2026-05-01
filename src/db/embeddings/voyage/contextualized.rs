//! Wire types for Voyage AI's `/v1/contextualizedembeddings` endpoint.
//!
//! The contextualized endpoint accepts a list of docs, where each doc is a
//! list of chunk strings. It returns per-chunk embeddings that are informed
//! by the full document context — cross-chunk attention lets later chunks
//! "see" earlier ones, producing richer vectors than independent embed calls.
//!
//! Reference: https://docs.voyageai.com/reference/contextualized-embeddings-api
//!
//! Privacy invariant: chunk text is NEVER logged anywhere in this module.

use serde::{Deserialize, Serialize};

pub const CONTEXTUALIZED_ENDPOINT: &str =
    "https://api.voyageai.com/v1/contextualizedembeddings";

// ── Request ───────────────────────────────────────────────────────────────────

/// Request body for `/v1/contextualizedembeddings`.
///
/// `inputs` is a list of documents, where each document is a list of chunks.
/// We always send exactly one document per call (`inputs.len() == 1`).
#[derive(Serialize)]
pub struct ContextualizedEmbedRequest<'a> {
    /// List of documents; each document is a list of chunk strings.
    pub inputs: Vec<Vec<&'a str>>,
    pub model: &'a str,
    pub input_type: &'a str,
    /// Pin to 1024 dimensions (Matryoshka — matches stored fingerprint).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dimension: Option<usize>,
}

// ── Response ──────────────────────────────────────────────────────────────────

/// Top-level response from `/v1/contextualizedembeddings`.
#[derive(Deserialize)]
pub struct ContextualizedEmbedResponse {
    /// One entry per input document.
    pub data: Vec<ContextualizedDocResult>,
}

/// Per-document result: contains one embedding per chunk in that document.
#[derive(Deserialize)]
pub struct ContextualizedDocResult {
    /// Chunk embeddings for this document, in input order.
    pub data: Vec<ContextualizedChunkEmbedding>,
    /// Zero-based index into the outer `inputs` array.
    #[allow(dead_code)]
    pub index: usize,
}

/// Per-chunk embedding within a document result.
#[derive(Deserialize)]
pub struct ContextualizedChunkEmbedding {
    pub embedding: Vec<f32>,
    /// Zero-based index of this chunk within its document.
    #[allow(dead_code)]
    pub index: usize,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contextualized_request_serializes_correctly() {
        let chunks = vec!["chunk one", "chunk two"];
        let req = ContextualizedEmbedRequest {
            inputs: vec![chunks],
            model: "voyage-context-3",
            input_type: "document",
            output_dimension: Some(1024),
        };
        let json = serde_json::to_value(&req).expect("serialize");
        assert_eq!(json["model"], "voyage-context-3");
        assert_eq!(json["input_type"], "document");
        assert_eq!(json["output_dimension"], 1024);
        assert_eq!(json["inputs"][0][0], "chunk one");
        assert_eq!(json["inputs"][0][1], "chunk two");
    }

    #[test]
    fn contextualized_request_omits_output_dimension_when_none() {
        let req = ContextualizedEmbedRequest {
            inputs: vec![vec!["hello"]],
            model: "voyage-context-3",
            input_type: "document",
            output_dimension: None,
        };
        let json = serde_json::to_value(&req).expect("serialize");
        assert!(json.get("output_dimension").is_none() || json["output_dimension"].is_null());
    }

    #[test]
    fn contextualized_response_deserializes_correctly() {
        let raw = r#"{
            "data": [
                {
                    "data": [
                        {"embedding": [0.1, 0.2, 0.3], "index": 0},
                        {"embedding": [0.4, 0.5, 0.6], "index": 1}
                    ],
                    "index": 0
                }
            ],
            "usage": {"total_tokens": 15}
        }"#;
        let resp: ContextualizedEmbedResponse = serde_json::from_str(raw).expect("deserialize");
        assert_eq!(resp.data.len(), 1);
        assert_eq!(resp.data[0].data.len(), 2);
        assert!((resp.data[0].data[0].embedding[0] - 0.1f32).abs() < f32::EPSILON);
        assert!((resp.data[0].data[1].embedding[0] - 0.4f32).abs() < f32::EPSILON);
    }

    #[test]
    fn contextualized_single_chunk_roundtrip() {
        let raw = r#"{
            "data": [{"data": [{"embedding": [0.9], "index": 0}], "index": 0}],
            "usage": {"total_tokens": 3}
        }"#;
        let resp: ContextualizedEmbedResponse = serde_json::from_str(raw).expect("deserialize");
        assert_eq!(resp.data[0].data[0].embedding[0], 0.9f32);
    }
}
