pub mod voyage;

use std::sync::Arc;

use crate::types::MemorySummary;

#[cfg(test)]
mod tests {
    use super::truncate_utf8;

    /// `truncate_utf8` must never split a multi-byte codepoint, must return
    /// valid UTF-8, and must not panic.
    #[test]
    fn truncate_utf8_at_multibyte_boundary() {
        // "héllo wörld 🌍" — contains 2-byte codepoints (é, ö) and a 4-byte codepoint (🌍).
        let input = "héllo wörld 🌍";

        // Find a byte offset that lands in the middle of a multibyte codepoint.
        // "h" = 1 byte, "é" = 2 bytes, so byte offset 2 is mid-codepoint for "é"
        // (byte 0='h', bytes 1-2='é'). Pick 2 to force the floor_char_boundary.
        let mid_codepoint_offset = 2usize;

        let result = truncate_utf8(input, mid_codepoint_offset);

        // 1. Result must be valid UTF-8 (String construction validates).
        let as_string = result.to_string();
        assert!(
            std::str::from_utf8(as_string.as_bytes()).is_ok(),
            "truncate_utf8 result must be valid UTF-8"
        );

        // 2. Result byte length must not exceed max_bytes.
        assert!(
            result.len() <= mid_codepoint_offset,
            "truncate_utf8 result must be <= max_bytes; got {} bytes",
            result.len()
        );

        // 3. Non-empty result with a safe offset (entire string fits).
        let safe_result = truncate_utf8(input, 1000);
        assert_eq!(safe_result, input, "when max_bytes >= len, returns full string");

        // 4. Zero max_bytes → empty string (no panic).
        let empty = truncate_utf8(input, 0);
        assert!(empty.is_empty(), "max_bytes=0 must return empty string");

        // 5. Boundary on a 4-byte emoji: 🌍 starts at some byte offset.
        //    Cut 1 byte into the emoji and verify result is valid UTF-8 and shorter.
        let emoji_input = "🌍 world";
        // 🌍 is 4 bytes; cut at byte 1 (inside the emoji).
        let cut = truncate_utf8(emoji_input, 1);
        assert!(
            std::str::from_utf8(cut.as_bytes()).is_ok(),
            "mid-emoji truncation must be valid UTF-8"
        );
        assert!(cut.is_empty(), "mid-emoji cut at byte 1 must yield empty (floor_char_boundary goes to 0)");
    }
}

/// Internal-only struct (not part of MCP wire shape).
/// Bundles a candidate with its raw content for cross-encoder rescoring.
pub(crate) struct RerankCandidate {
    pub summary: MemorySummary,
    pub content: Option<String>,
}

/// Truncate `text` to at most `max_bytes` bytes, UTF-8 safe via `floor_char_boundary`.
pub(crate) fn truncate_utf8(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        text
    } else {
        &text[..text.floor_char_boundary(max_bytes)]
    }
}

/// Cross-encoder reranker — stage 3.5 of the retrieval pipeline (ADR-017).
///
/// Soft-fail contract: every `rerank` call returns input order on any error.
/// Callers must not propagate reranker failures.
///
/// `Clone` is cheap — Voyage variant wraps inner state in `Arc`.
#[derive(Clone)]
pub enum Reranker {
    Voyage(Arc<voyage::VoyageReranker>, crate::types::RerankerPolicy),
}

impl Reranker {
    /// Returns the configured gating policy (ADR-022).
    pub fn policy(&self) -> crate::types::RerankerPolicy {
        match self {
            Self::Voyage(_, p) => *p,
        }
    }

    /// Reranks `candidates` against `query`.
    ///
    /// Each candidate's text is built via `content` if present, `summary` otherwise,
    /// truncated to 3200 chars. Returns `MemorySummary` in relevance order (content
    /// dropped — wire shape unchanged). On any error the input order is returned
    /// unchanged (soft-fail per ADR-017).
    pub(crate) async fn rerank(&self, query: &str, candidates: Vec<RerankCandidate>) -> Vec<MemorySummary> {
        match self {
            Self::Voyage(r, _) => r.rerank(query, candidates).await,
        }
    }
}
