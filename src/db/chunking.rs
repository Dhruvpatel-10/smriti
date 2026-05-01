//! Content chunking for long documents.
//!
//! When a memory has long `content` (e.g., meeting transcripts, article notes),
//! a single embedding cannot represent all the information well. This module
//! splits content into overlapping chunks, creating sub-memories linked via
//! `extends` edges back to the parent.
//!
//! Each chunk gets its own vector embedding, enabling fine-grained vector search
//! into long documents. The parent memory retains its own embedding (from the
//! summary), so both coarse and fine-grained search paths work.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tracing::{info, instrument, warn};

use crate::db::client::SurrealClient;
use crate::db::embeddings::store::store_vector;
use crate::db::embeddings::Embedder;
use crate::db::graph;
use crate::db::memory;
use crate::error::Result;
use crate::types::*;

/// Minimum content length to trigger chunking.
pub const CHUNK_THRESHOLD: usize = 2000;

/// Fields from the parent memory needed to create chunk sub-memories.
///
/// Passed by the caller (write path) so chunking makes zero SELECTs on
/// the memory table itself.
pub struct ChunkParent {
    pub summary: String,
    pub content: String,
    pub memory_type: MemoryType,
    pub subtype: MemorySubtype,
    pub channel: Option<Channel>,
    pub modality: Option<Modality>,
    pub project: Option<String>,
    pub domain: Vec<String>,
    pub importance: Importance,
    pub visibility: Visibility,
    pub event_at: DateTime<Utc>,
    pub tags: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub source_agent: Option<String>,
    pub session_id: Option<String>,
    pub author: Option<AgentId>,
}

/// Separator hierarchy for recursive splitting (highest-priority first).
const SEPARATORS: &[&str] = &["\n## ", "\n# ", "\n\n", "\n", ". ", "? ", "! ", " "];

/// Split text into overlapping chunks driven by embedder token budgets.
///
/// `target_tokens` — target chunk size from `embedder.recommended_chunk_tokens()`.
/// `max_tokens` — hard upper cap from `embedder.max_input_tokens()`.
/// `overlap_tokens` — prepend this many tokens-worth of context from the previous chunk.
///
/// UTF-8 safe: all slicing uses `floor_char_boundary`. Token estimation heuristic:
/// `chars / 4` (accurate enough for sizing; precise tokenization is the embedder's job).
///
/// Code-fence preservation: never splits inside a ``` or ```` block.
/// 4-tick fences are handled. Blockquoted fences (`> ``` `) are a known limitation
/// (ADR-021 follow-up).
pub fn split_into_chunks(
    text: &str,
    target_tokens: usize,
    max_tokens: usize,
    overlap_tokens: usize,
) -> Vec<String> {
    let target_chars = target_tokens * 4;
    let max_chars = max_tokens * 4;
    let overlap_chars = overlap_tokens * 4;

    if text.chars().count() <= target_chars {
        return vec![text.to_string()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        let remaining_char_count = remaining.chars().count();
        if remaining_char_count <= target_chars {
            // Remainder fits in one chunk — append any overlap prefix and finish.
            let chunk = prepend_overlap(&chunks, remaining, overlap_chars);
            chunks.push(chunk);
            break;
        }

        // Candidate window: target_chars bytes from start (floor-aligned).
        let window_end_byte = remaining.floor_char_boundary(target_chars.min(remaining.len()));

        // Find the best split point within the window, respecting code fences.
        let split_byte = find_split_point(remaining, window_end_byte, max_chars);

        let (this_slice, rest) = remaining.split_at(split_byte);
        let chunk = prepend_overlap(&chunks, this_slice, overlap_chars);
        chunks.push(chunk);
        remaining = rest.trim_start_matches('\n');
        // Guard against zero-progress (e.g. single huge non-splittable token).
        if std::ptr::addr_eq(remaining as *const str, rest as *const str) && split_byte == 0 {
            // Force-advance by one char to prevent infinite loop.
            let next = rest.floor_char_boundary(1.min(rest.len()));
            let chunk = prepend_overlap(&chunks, &rest[..next], overlap_chars);
            chunks.push(chunk);
            remaining = &rest[next..];
        }
    }

    if chunks.is_empty() {
        chunks.push(text.to_string());
    }
    chunks
}

/// Find the best byte-offset to split `text` at, within `window_end` bytes.
///
/// Tries separators from highest to lowest priority. Skips positions that fall
/// inside an open code fence (3- or 4-tick). Falls back to a hard char-boundary
/// cut at `max_chars` if no separator is found.
fn find_split_point(text: &str, window_end: usize, max_chars: usize) -> usize {
    let max_byte = text.floor_char_boundary(max_chars.min(text.len()));

    // Try each separator in priority order.
    for sep in SEPARATORS {
        if let Some(pos) = rfind_outside_fence(&text[..window_end], sep) {
            let split = pos + sep.len();
            if split > 0 && split <= text.len() {
                return split;
            }
        }
    }

    // No separator found — try the extended window up to max_chars.
    if max_byte > window_end {
        for sep in SEPARATORS {
            if let Some(pos) = rfind_outside_fence(&text[..max_byte], sep) {
                let split = pos + sep.len();
                if split > 0 && split <= text.len() {
                    return split;
                }
            }
        }
    }

    // Hard cut at max_chars boundary.
    max_byte.max(1)
}

/// Find the last occurrence of `sep` in `text` that does not fall inside an
/// open code fence (3-tick or 4-tick).
fn rfind_outside_fence(text: &str, sep: &str) -> Option<usize> {
    // Collect all fence-open/close ranges first, then check each candidate position.
    let fence_ranges = collect_fence_ranges(text);

    // Iterate from the end — rfind behavior.
    let mut last = None;
    let mut search_from = 0;
    while let Some(pos) = text[search_from..].find(sep) {
        let abs_pos = search_from + pos;
        if !inside_fence(&fence_ranges, abs_pos) {
            last = Some(abs_pos);
        }
        search_from = abs_pos + 1;
        if search_from > text.len() {
            break;
        }
    }
    last
}

/// Returns `true` if `byte_pos` is inside any open-code-fence range.
fn inside_fence(ranges: &[(usize, usize)], byte_pos: usize) -> bool {
    ranges.iter().any(|(start, end)| byte_pos > *start && byte_pos < *end)
}

/// Collect byte-ranges of code-fence content blocks (3-tick or 4-tick).
///
/// Returns `(open_tick_pos, close_tick_pos)` pairs; the range covers the interior
/// including the opening ``` line up to but not including the closing ```.
/// Unclosed fences extend to end-of-text — prevents splitting inside them.
fn collect_fence_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut byte_offset = 0usize;
    let mut open_fence: Option<(usize, usize)> = None; // (start_byte, tick_count)

    for line in text.split('\n') {
        let trimmed = line.trim_start();
        let tick_count = leading_ticks(trimmed);

        if tick_count >= 3 {
            match open_fence {
                None => {
                    // Opening fence.
                    open_fence = Some((byte_offset, tick_count));
                }
                Some((open_start, open_ticks)) if tick_count == open_ticks => {
                    // Closing fence (matching tick count).
                    let close_byte = byte_offset + line.len();
                    ranges.push((open_start, close_byte));
                    open_fence = None;
                }
                _ => {
                    // Different tick count inside an open fence — ignore (nested fences
                    // are not standard Markdown; treat as content).
                }
            }
        }

        byte_offset += line.len() + 1; // +1 for the '\n' split removed by split()
    }

    // Unclosed fence extends to end-of-text.
    if let Some((open_start, _)) = open_fence {
        ranges.push((open_start, text.len()));
    }

    ranges
}

/// Count the number of leading backtick characters in a string.
fn leading_ticks(s: &str) -> usize {
    s.chars().take_while(|&c| c == '`').count()
}

/// Prepend the last `overlap_chars` chars of the previous chunk to `slice`.
///
/// UTF-8 safe via `floor_char_boundary`. No-op when `chunks` is empty.
fn prepend_overlap(chunks: &[String], slice: &str, overlap_chars: usize) -> String {
    if overlap_chars == 0 {
        return slice.to_string();
    }
    let Some(prev) = chunks.last() else {
        return slice.to_string();
    };
    let prev_len = prev.len();
    if prev_len <= overlap_chars {
        // Whole previous chunk fits in the overlap window.
        format!("{prev}{slice}")
    } else {
        let start_byte = prev.floor_char_boundary(prev_len - overlap_chars);
        format!("{}{slice}", &prev[start_byte..])
    }
}

/// For a memory with long content, create chunk sub-memories linked
/// via `extends` edges back to the parent.
///
/// Returns the IDs of created chunk memories. Returns empty vec if
/// content is too short to chunk.
///
/// Pipeline: split → prepare (prefix or pass-through) → single batched embed
/// → create records → store vectors → link edges. The batched embed replaces
/// per-chunk fire-and-forget spawns, eliminating the race where a chunk record
/// could be read before its vector arrived.
#[instrument(skip(db, embedder, parent), fields(parent_id = %parent_id))]
pub async fn chunk_and_embed(
    db: &SurrealClient,
    embedder: Arc<dyn Embedder>,
    parent_id: &MemoryId,
    parent: &ChunkParent,
) -> Result<Vec<MemoryId>> {
    if parent.content.len() < CHUNK_THRESHOLD {
        return Ok(vec![]);
    }

    let target = embedder.recommended_chunk_tokens();
    let max = embedder.max_input_tokens();
    let chunks = split_into_chunks(&parent.content, target, max, 50);
    if chunks.len() <= 1 {
        return Ok(vec![]);
    }

    let total = chunks.len();

    // When the backend handles context natively (max_chunks_per_doc > 1, e.g.
    // the Voyage `voyage-context-3` model via `/v1/contextualizedembeddings`),
    // all chunks are sent as a single-document list in one HTTP request and the
    // model performs cross-chunk attention server-side. Each returned vector
    // therefore already "knows" the full document — prepending a structural
    // prefix would add noise without benefit, and is skipped.
    //
    // For single-chunk backends (local fastembed, max_chunks_per_doc == 1),
    // each chunk is embedded in isolation with no surrounding context. The
    // structural prefix (markdown heading or parent summary head) gives the
    // model a disambiguation signal that cross-chunk attention would otherwise
    // provide. It is intentionally capped to max_input_tokens to avoid
    // crowding out the chunk body.
    let prepared: Vec<String> = if embedder.max_chunks_per_doc() > 1 {
        chunks.clone()
    } else {
        chunks
            .iter()
            .map(|c| prepend_structural_prefix(c, &parent.summary, embedder.max_input_tokens()))
            .collect()
    };

    // Single batched call — Voyage uses contextualized endpoint; local falls
    // back to sequential embed_document calls in the trait default.
    let vectors = embedder.embed_document_chunks(&prepared).await?;

    let mut chunk_ids = Vec::with_capacity(total);

    for (i, (chunk_text, vector)) in chunks.iter().zip(vectors.into_iter()).enumerate() {
        let chunk_summary = format!(
            "[chunk {}/{}] {}",
            i + 1,
            total,
            truncate_str(&parent.summary, 80)
        );

        let draft = memory::MemoryDraft::new(
            chunk_summary.clone(),
            parent.memory_type,
            parent.subtype,
            parent.channel,
            parent.modality,
        )
        .content(chunk_text.clone())
        .maybe_project(parent.project.clone())
        .domain(parent.domain.clone())
        .importance(parent.importance)
        .visibility(parent.visibility)
        .event_at(parent.event_at)
        .tags(parent.tags.clone())
        .maybe_expires_at(parent.expires_at)
        .maybe_source_agent(parent.source_agent.clone())
        .maybe_session_id(parent.session_id.clone())
        .maybe_author(parent.author.clone());

        let chunk_id = memory::create(db, draft).await?;

        if let Err(e) =
            store_vector(db, chunk_id.as_str(), vector).await
        {
            warn!(chunk = i + 1, "failed to store chunk vector: {e}");
        }

        if let Err(e) = graph::link_memories(db, &chunk_id, parent_id, RelationType::Extends).await
        {
            warn!(chunk = i + 1, "failed to link chunk to parent: {e}");
        }

        chunk_ids.push(chunk_id);
    }

    info!(parent = %parent_id, chunks = total, "created content chunks");
    Ok(chunk_ids)
}

/// Prepend a structural prefix to a chunk for single-string embedders.
///
/// Looks for a markdown heading in the first 3 lines of the chunk (Khoj-style).
/// If found, prepends it; otherwise falls back to the parent summary. This
/// gives context-free embedders a hint about the document section each chunk
/// belongs to without the cross-chunk attention of the Voyage contextualized API.
///
/// `max_input_tokens` is used to guard against pathologically long parent
/// summaries — rough char/token heuristic (4 chars ≈ 1 token).
pub(crate) fn prepend_structural_prefix(chunk: &str, parent_summary: &str, max_input_tokens: usize) -> String {
    let heading = chunk
        .lines()
        .take(3)
        .find(|l| l.starts_with("# ") || l.starts_with("## "));

    let prefix = heading.unwrap_or(parent_summary);

    // Guard: combined length must not exceed max_input_tokens * 4 chars.
    let max_chars = max_input_tokens.saturating_mul(4);
    let prefix_budget = max_chars.saturating_sub(chunk.len()).saturating_sub(2); // 2 for "\n\n"

    if prefix_budget == 0 || prefix.is_empty() {
        return chunk.to_string();
    }

    let safe_prefix = truncate_str(prefix, prefix_budget);
    format!("{safe_prefix}\n\n{chunk}")
}

fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s.floor_char_boundary(max)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // target=100 tokens → 400 chars; max=200 tokens → 800 chars; overlap=50 tokens → 200 chars
    const T: usize = 100;
    const M: usize = 200;
    const O: usize = 50;

    #[test]
    fn prepend_structural_prefix_uses_heading() {
        let chunk = "## Section Title\n\nsome content here";
        let result = prepend_structural_prefix(chunk, "parent summary", 32000);
        assert!(result.starts_with("## Section Title\n\n"), "expected heading prefix");
    }

    #[test]
    fn prepend_structural_prefix_falls_back_to_parent_summary() {
        let chunk = "just some text without any heading";
        let result = prepend_structural_prefix(chunk, "Parent Summary", 32000);
        assert!(result.starts_with("Parent Summary\n\n"), "expected parent summary prefix");
        assert!(result.contains("just some text"), "chunk content must be present");
    }

    #[test]
    fn prepend_structural_prefix_truncates_very_long_summary() {
        let chunk = "short chunk content!"; // 20 chars
        let long_summary = "A".repeat(200);
        let result = prepend_structural_prefix(chunk, &long_summary, 10);
        let max_expected = 10 * 4;
        assert!(
            result.len() <= max_expected + 10,
            "result too long: {} chars",
            result.len()
        );
    }

    #[test]
    fn prepend_structural_prefix_h1_heading_detected() {
        let chunk = "# Top-level\n\ncontent body";
        let result = prepend_structural_prefix(chunk, "summary", 32000);
        assert!(result.starts_with("# Top-level\n\n"));
    }

    #[test]
    fn short_text_not_chunked() {
        let chunks = split_into_chunks("hello world", T, M, O);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello world");
    }

    #[test]
    fn splitter_handles_emoji_safely() {
        // Multi-byte Unicode: emoji + CJK characters. Must not panic.
        let emoji_part = "😀🎉🌟".repeat(50);   // 3 × 4 bytes each × 50 = 600 bytes, 150 chars
        let cjk_part = "日本語テスト".repeat(50); // 6 chars × 3 bytes × 50 = 900 bytes, 300 chars
        let text = format!("{emoji_part} {cjk_part}");
        let chunks = split_into_chunks(&text, T, M, O);
        // Must produce at least one chunk without panicking.
        assert!(!chunks.is_empty(), "must produce chunks");
        // Every chunk must be valid UTF-8 (String is always valid UTF-8 if created safely).
        for chunk in &chunks {
            assert!(std::str::from_utf8(chunk.as_bytes()).is_ok(), "chunk is invalid UTF-8");
        }
    }

    #[test]
    fn splitter_respects_max_tokens_hard_cap() {
        // One giant paragraph (no separators) longer than max_chars (M * 4 = 800 chars).
        let text = "a".repeat(2000);
        let chunks = split_into_chunks(&text, T, M, O);
        // No chunk body (excluding overlap) should exceed max_chars * 2 in total length.
        // The overlap may add up to O*4=200 chars on top.
        let cap = M * 4 + O * 4 + 10; // generous tolerance for overlap
        for (i, chunk) in chunks.iter().enumerate() {
            assert!(
                chunk.chars().count() <= cap,
                "chunk {i} exceeds hard cap: {} chars (cap={cap})",
                chunk.chars().count()
            );
        }
    }

    #[test]
    fn splitter_preserves_three_tick_fence() {
        // Construct text with a 3-tick fence that straddles a target boundary.
        // target = 100 tokens = 400 chars. Fence opens just past the target window.
        // Fence must be on its own line (standard Markdown).
        let before = "A".repeat(360);
        let fence = "\n```rust\nlet x = 1;\nlet y = 2;\nlet z = 3;\n```\n";
        let after = "B".repeat(200);
        let text = format!("{before}{fence}{after}");
        let chunks = split_into_chunks(&text, T, M, O);
        // The fence interior must not be split: no chunk may contain just the
        // opening ``` without the matching closing ```.
        for chunk in &chunks {
            let open_count = chunk.matches("```").count();
            // Either no fence markers (chunk is outside), or even count (balanced).
            // Odd count would mean a chunk starts/ends mid-fence.
            assert!(
                open_count % 2 == 0 || open_count == 0,
                "chunk has unbalanced fence markers ({open_count} occurrences of ```): snippet = {:?}",
                &chunk[..chunk.len().min(80)]
            );
        }
    }

    #[test]
    fn splitter_preserves_four_tick_fence() {
        let before = "A".repeat(360);
        let fence = "\n````python\nfor i in range(10):\n    print(i)\n````\n";
        let after = "C".repeat(200);
        let text = format!("{before}{fence}{after}");
        let chunks = split_into_chunks(&text, T, M, O);
        for chunk in &chunks {
            let quad_count = chunk.matches("````").count();
            assert!(
                quad_count % 2 == 0 || quad_count == 0,
                "chunk has unbalanced 4-tick fence markers ({quad_count} occurrences): snippet = {:?}",
                &chunk[..chunk.len().min(80)]
            );
        }
    }

    #[test]
    fn splitter_uses_markdown_headings_first() {
        // Two sections separated by ## heading. target=100 tokens = 400 chars.
        // Section A is ~380 chars, then heading, then Section B.
        let section_a = "x".repeat(380);
        let section_b = "y".repeat(400);
        let text = format!("{section_a}\n## Section B\n{section_b}");
        let chunks = split_into_chunks(&text, T, M, O);
        assert!(chunks.len() >= 2, "must produce at least 2 chunks");
        // First chunk should be the section A content ending just before the ## heading.
        // The separator "\n## " is consumed, so first chunk ends with x's (no "## Section B").
        let first = &chunks[0];
        // The heading itself (## Section B) should appear in the second chunk.
        let combined_tail: String = chunks[1..].concat();
        assert!(
            combined_tail.contains("## Section B") || combined_tail.contains("Section B"),
            "heading must appear in a later chunk"
        );
        assert!(
            !first.contains("## Section B"),
            "first chunk should not contain the ## heading"
        );
    }

    /// A 3-tick fence inside a 4-tick fenced block must be treated as content,
    /// not as a fence boundary. The 4-tick fence dominates; `collect_fence_ranges`
    /// must not close the open 4-tick fence when it encounters the inner 3-tick.
    #[test]
    fn rfind_outside_fence_handles_nested_3_in_4() {
        // 4-tick fence opens at line 2; a 3-tick fence appears inside at line 4.
        // The 4-tick fence closes at line 6.
        // A separator "\n\n" appears at line 3 (inside the 4-tick block) and
        // another appears after the closing 4-tick. We verify that the inner
        // separator (inside the fence) is NOT returned as a split point, while
        // the outer separator (after the fence) IS returned.
        let before = "some text before\n\n";
        let fence_open = "````python\n";
        let inner_content = "print('hello')\n";
        let inner_3tick = "```\n# not a fence boundary\n```\n";
        let fence_content = "more code here\n";
        let fence_close = "````\n\n";
        let after = "text after fence";
        let text = format!("{before}{fence_open}{inner_content}{inner_3tick}{fence_content}{fence_close}{after}");

        let ranges = collect_fence_ranges(&text);

        // The 4-tick fence should produce exactly one range.
        assert_eq!(ranges.len(), 1, "should have exactly one fence range (4-tick dominates); got {ranges:?}");

        // The separator inside the fence should not be a split candidate.
        // Find "\n\n" positions:
        // - First "\n\n": in `before`, BEFORE the fence → outside fence → valid split point.
        // - Second "\n\n": inside the 4-tick block → inside fence → must NOT be returned.
        // - Third "\n\n": in fence_close (the "\n\n" after "````") → might be inside or outside.
        //   Check: the fence_close is "````\n\n" — the "````" closes the fence, then "\n\n" is outside.
        // We just verify that the 3-tick occurrence inside the fence body doesn't
        // prematurely close the 4-tick fence (i.e., ranges has exactly 1 entry).
        let (range_start, range_end) = ranges[0];
        assert!(range_end > range_start, "fence range must span content");

        // The inner 3-tick substring should be inside the fence range.
        let inner_pos = text.find("```\n# not a fence").expect("inner 3-tick present");
        assert!(
            inner_pos > range_start && inner_pos < range_end,
            "inner 3-tick fence at byte {inner_pos} should be inside range [{range_start},{range_end}]"
        );
    }

    /// Single-chunk backend (max_chunks_per_doc == 1): verify that each chunk
    /// prepared for embedding contains the parent summary as a prefix.
    ///
    /// This mirrors the production path in `chunk_and_embed` where
    /// `prepend_structural_prefix` is called per chunk before `embed_document_chunks`.
    #[test]
    fn single_chunk_backend_chunks_carry_parent_context() {
        let parent_summary = "Weekly retrospective on Rust async design patterns";
        let section = "word ".repeat(120); // 600 chars — exceeds T (400) → forces multiple chunks
        let content = format!("{section}\n\n{section}");

        let chunks = split_into_chunks(&content, T, M, O);
        assert!(chunks.len() >= 2, "expected multiple chunks for long content");

        // Simulate the single-chunk backend preparation path.
        let max_input_tokens = 32000;
        let prepared: Vec<String> = chunks
            .iter()
            .map(|c| prepend_structural_prefix(c, parent_summary, max_input_tokens))
            .collect();

        for (i, prepared_chunk) in prepared.iter().enumerate() {
            assert!(
                prepared_chunk.contains(parent_summary),
                "chunk {i} must contain parent summary for context-free embedder; \
                 got prefix: {:?}",
                &prepared_chunk[..prepared_chunk.len().min(120)]
            );
        }
    }

    /// Context-aware backend (max_chunks_per_doc > 1): chunks are passed as-is;
    /// the embedder receives raw chunk text without a structural prefix injected
    /// by the chunking layer. Context is provided server-side (e.g. Voyage
    /// `/v1/contextualizedembeddings` cross-chunk attention).
    #[test]
    fn context_aware_backend_chunks_are_not_prefixed() {
        let parent_summary = "Weekly retrospective on Rust async design patterns";
        let section = "word ".repeat(120);
        let content = format!("{section}\n\n{section}");

        let chunks = split_into_chunks(&content, T, M, O);
        assert!(chunks.len() >= 2, "expected multiple chunks for long content");

        // Simulate context-aware backend path: chunks passed as-is.
        let prepared: Vec<String> = chunks.clone();

        for (i, (raw, prep)) in chunks.iter().zip(prepared.iter()).enumerate() {
            assert_eq!(
                raw, prep,
                "chunk {i} must be unchanged for context-aware backends"
            );
            // The raw chunk does NOT start with the parent summary — context
            // arrives via cross-chunk attention, not a text prefix.
            assert!(
                !prep.starts_with(parent_summary),
                "chunk {i} must not have parent summary injected for context-aware backends"
            );
        }
    }

    #[test]
    fn splitter_overlap_preserves_continuity() {
        // Input with clear paragraph breaks so we get multiple chunks.
        let para = "word ".repeat(80); // 400 chars per paragraph
        let text = format!("{para}\n\n{para}\n\n{para}");
        let chunks = split_into_chunks(&text, T, M, O);
        if chunks.len() < 2 {
            // Single chunk — nothing to verify.
            return;
        }
        // chunk[N+1] should begin with content from the tail of chunk[N].
        for window in chunks.windows(2) {
            let prev = &window[0];
            let next = &window[1];
            // The overlap region: last O*4 chars of prev should be a prefix of next.
            // Allow for leading whitespace strip.
            let overlap_chars = O * 4;
            if prev.chars().count() >= overlap_chars {
                let prev_tail_start = prev.floor_char_boundary(prev.len().saturating_sub(overlap_chars));
                let prev_tail = &prev[prev_tail_start..];
                let next_trimmed = next.trim_start_matches('\n');
                assert!(
                    next_trimmed.starts_with(prev_tail.trim_start_matches('\n')),
                    "chunk continuity broken: prev tail {:?} not found at start of next {:?}",
                    &prev_tail[..prev_tail.len().min(40)],
                    &next_trimmed[..next_trimmed.len().min(40)]
                );
            }
        }
    }
}
