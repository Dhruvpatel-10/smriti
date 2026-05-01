use std::sync::Arc;

use tracing::{instrument, warn};

use crate::db::client::SurrealClient;
use crate::db::dedup;
use crate::db::graph;
use crate::error::{KbError, Result};
use crate::shutdown::TaskRegistry;
use crate::types::{MemoryId, RelationType};

/// Similarity threshold for creating `relates` edges (same project/domain).
const RELATES_THRESHOLD: f64 = 0.75;
/// Similarity threshold for cross-domain `relates` edges (higher bar).
const CROSS_DOMAIN_THRESHOLD: f64 = 0.85;
/// Similarity threshold for creating `extends` edges (same project only).
const EXTENDS_THRESHOLD: f64 = 0.90;
/// Maximum auto-linked similarity edges per memory.
const MAX_SIMILARITY_EDGES: usize = 8;
/// DoS guard: cap on entity name candidates extracted from a single summary.
const MAX_CANDIDATES: usize = 64;

/// Auto-link a newly stored memory to similar existing memories via graph edges.
///
/// Thresholds (post-DA review):
/// - similarity > 0.90 AND same project → `extends` edge
/// - similarity > 0.75 (same project/domain) or > 0.85 (cross-domain) → `relates` edge
/// - Capped at 8 similarity edges per memory
///
/// Runs async after memory creation. Errors are logged, not propagated.
#[instrument(skip(db, embedding))]
pub async fn auto_link_similar(
    db: &SurrealClient,
    memory_id: &MemoryId,
    memory_project: Option<&str>,
    embedding: Vec<f32>,
) -> Result<u32> {
    // Fetch more candidates than the cap so we can filter by threshold.
    // find_similar returns (id, score, project) — use project directly to
    // avoid N+1 check_same_project queries.
    let similar = dedup::find_similar(db, embedding, memory_id, RELATES_THRESHOLD, 15).await?;

    let mut links_created = 0u32;

    for (similar_id, score, similar_project) in similar {
        if links_created as usize >= MAX_SIMILARITY_EDGES {
            break;
        }

        let same_project = memory_project
            .is_some_and(|proj| similar_project.as_deref() == Some(proj));

        // Determine relation type and whether to create the edge
        let relation = if score >= EXTENDS_THRESHOLD && same_project {
            Some(RelationType::Extends)
        } else if score >= RELATES_THRESHOLD && same_project {
            Some(RelationType::Relates)
        } else if score >= CROSS_DOMAIN_THRESHOLD {
            // Cross-domain links require higher confidence
            Some(RelationType::Relates)
        } else {
            None // Below threshold for cross-domain
        };

        if let Some(rel) = relation {
            let similar_mem_id = MemoryId::from(similar_id.clone());
            match graph::link_memories(db, memory_id, &similar_mem_id, rel).await {
                Ok(()) => links_created += 1,
                Err(e) => warn!(from = %memory_id, to = %similar_id, "auto-link failed: {e}"),
            }
        }
    }

    Ok(links_created)
}

/// Scan a summary for known entity names and create `involves` edges.
///
/// Extracts candidate proper-noun tokens from the summary, then queries the
/// FTS index (`idx_entity_name_ft`, migration 005) with `@@` for each
/// candidate. The class tokenizer + lowercase filter means "Alex" will not
/// match "Alexander" — they are distinct tokens.
#[instrument(skip(db, summary))]
pub async fn match_known_entities(
    db: &SurrealClient,
    memory_id: &MemoryId,
    summary: &str,
) -> Result<u32> {
    let mut candidates = extract_candidates(summary);

    // DoS guard: drop excess candidates rather than issuing unbounded queries.
    if candidates.len() > MAX_CANDIDATES {
        let dropped = candidates.len() - MAX_CANDIDATES;
        warn!(id = %memory_id, dropped, "entity candidates truncated");
        candidates.truncate(MAX_CANDIDATES);
    }

    if candidates.is_empty() {
        return Ok(0);
    }

    let mut links_created = 0u32;

    for candidate in candidates {
        let mut result = db
            .query("SELECT id, name FROM entity WHERE name @@ $candidate LIMIT 1")
            .bind(("candidate", candidate))
            .await
            .map_err(|e| KbError::db("entity FTS lookup", e))?;

        let rows: Vec<serde_json::Value> = result
            .take(0)
            .unwrap_or_default();

        for row in rows {
            if let Some(name) = row.get("name").and_then(|v| v.as_str()) {
                match graph::link_entity(db, memory_id, name).await {
                    Ok(()) => links_created += 1,
                    Err(e) => warn!(id = %memory_id, "entity auto-link failed: {e}"),
                }
            }
        }
    }

    Ok(links_created)
}

/// Extract proper-noun candidate strings from a summary for FTS entity lookup.
///
/// Yields individual capitalized words (≥ 2 chars) and consecutive capitalized
/// bigrams, both as standalone candidates. Tokens may contain internal
/// apostrophes or hyphens (e.g. "O'Brien", "Mary-Anne"). Purely heuristic;
/// sufficient for names stored in the entity table.
fn extract_candidates(summary: &str) -> Vec<String> {
    // Single pass: build all_words (every normalized token) and partition into
    // proper-noun words (starts uppercase, all alphabetic/apostrophe/hyphen, ≥2 chars).
    let all_words: Vec<String> = summary
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-')
                .to_string()
        })
        .collect();

    let words: Vec<&str> = all_words
        .iter()
        .map(|w| w.as_str())
        .filter(|w| {
            w.len() >= 2
                && w.starts_with(|c: char| c.is_uppercase())
                && w.chars().all(|c| c.is_alphabetic() || c == '\'' || c == '-')
        })
        .collect();

    let mut candidates: Vec<String> = Vec::with_capacity(words.len() * 2);

    // Single-word candidates.
    for w in &words {
        candidates.push(w.to_string());
    }

    // Bigram candidates from consecutive positions in the normalized token list.
    let word_set: std::collections::HashSet<&str> = words.iter().copied().collect();
    let mut i = 0;
    while i + 1 < all_words.len() {
        let a = all_words[i].as_str();
        let b = all_words[i + 1].as_str();
        if word_set.contains(a) && word_set.contains(b) {
            candidates.push(format!("{a} {b}"));
            i += 2;
        } else {
            i += 1;
        }
    }

    // Deduplicate while preserving order.
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|c| seen.insert(c.clone()));

    candidates
}

// ═══════════════════════════════════════════════════════════════════════════
// Unit tests for extract_candidates (covers word-boundary + length guard)
// ═══════════════════════════════════════════════════════════════════════════
#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::extract_candidates;

    /// A capitalised name at the very start of a summary is extracted.
    #[test]
    fn name_at_start_matches() {
        let candidates = extract_candidates("Alice is here at the meeting");
        assert!(
            candidates.contains(&"Alice".to_string()),
            "expected 'Alice' in {candidates:?}"
        );
    }

    /// A capitalised name at the end of a summary is extracted.
    #[test]
    fn name_at_end_matches() {
        let candidates = extract_candidates("talked to Alice");
        assert!(
            candidates.contains(&"Alice".to_string()),
            "expected 'Alice' in {candidates:?}"
        );
    }

    /// A lowercase substring that happens to appear inside a word is NOT emitted
    /// because `extract_candidates` only yields tokens that start with uppercase
    /// and are purely alphabetic — "ain" never appears as a candidate.
    #[test]
    fn name_embedded_does_not_match() {
        let candidates = extract_candidates("Plaintext is a format");
        // "ain" should not be a standalone candidate
        assert!(
            !candidates.contains(&"ain".to_string()),
            "did not expect 'ain' in {candidates:?}"
        );
    }

    /// Names ≥ 2 characters are extracted; single-char tokens are still filtered.
    #[test]
    fn short_name_extracted() {
        // "Bob" is 3 chars — now above the `w.len() >= 2` floor.
        let candidates = extract_candidates("Bob said something important today");
        assert!(
            candidates.contains(&"Bob".to_string()),
            "expected 'Bob' (3 chars) in {candidates:?}"
        );
    }

    /// 2-char person name "Li" is extracted.
    #[test]
    fn two_char_name_li() {
        let candidates = extract_candidates("Li confirmed the meeting time");
        assert!(
            candidates.contains(&"Li".to_string()),
            "expected 'Li' in {candidates:?}"
        );
    }

    /// 3-char person name "Anya" (actually 4 chars, but also "Eve" at 3).
    #[test]
    fn three_char_name_eve() {
        let candidates = extract_candidates("Eve approved the change");
        assert!(
            candidates.contains(&"Eve".to_string()),
            "expected 'Eve' in {candidates:?}"
        );
    }

    /// 4-char name "Anya" still works.
    #[test]
    fn four_char_name_anya() {
        let candidates = extract_candidates("Anya reviewed the document");
        assert!(
            candidates.contains(&"Anya".to_string()),
            "expected 'Anya' in {candidates:?}"
        );
    }

    /// Apostrophe-containing name "O'Brien" survives as a single token.
    #[test]
    fn apostrophe_name_obrien() {
        let candidates = extract_candidates("talked to O'Brien about the plan");
        assert!(
            candidates.contains(&"O'Brien".to_string()),
            "expected \"O'Brien\" in {candidates:?}"
        );
    }

    /// Hyphenated name "Mary-Anne" survives as a single token.
    #[test]
    fn hyphen_name_mary_anne() {
        let candidates = extract_candidates("Mary-Anne joined the call");
        assert!(
            candidates.contains(&"Mary-Anne".to_string()),
            "expected 'Mary-Anne' in {candidates:?}"
        );
    }

    /// Mixed-case name "McDonald" is extracted (leading uppercase, internal mixed case).
    #[test]
    fn mixed_case_mcdonald() {
        let candidates = extract_candidates("McDonald presented the findings");
        assert!(
            candidates.contains(&"McDonald".to_string()),
            "expected 'McDonald' in {candidates:?}"
        );
    }

    /// Summaries containing Unicode characters must not panic.
    /// Current semantics: "Alice" (ASCII, ≥4 chars, uppercase-start, alphabetic)
    /// is extracted even when adjacent to a Unicode word.
    #[test]
    fn name_adjacent_to_unicode_char_does_not_panic() {
        // Must not panic — assert observed behaviour (Alice is extracted).
        let candidates = extract_candidates("Ünterground Alice spoke today");
        // The key invariant: no panic.  Also verify Alice is picked up since it
        // satisfies all filters (starts uppercase, all alphabetic ASCII, len ≥ 4).
        assert!(
            candidates.contains(&"Alice".to_string()),
            "expected 'Alice' in {candidates:?}"
        );
    }
}

/// Spawn auto-linking with a pre-computed vector (no double-embed).
/// Combines embedding-based similarity linking + entity name matching.
/// The spawn is registered in `tasks` so shutdown can drain it.
///
/// Caller must `.await` this function — it registers directly into `tasks`
/// without an intermediate `tokio::spawn`, so the registered future is never
/// untracked even if the caller drops the handle before the inner work begins.
pub async fn spawn_auto_link_with_vector(
    db: SurrealClient,
    memory_id: MemoryId,
    memory_project: Option<String>,
    summary: String,
    vector: Vec<f32>,
    tasks: Arc<TaskRegistry>,
) {
    tasks.spawn(async move {
        // Entity name matching (doesn't need embedding)
        let entity_matches = match match_known_entities(&db, &memory_id, &summary).await {
            Ok(count) => {
                if count > 0 {
                    tracing::info!(id = %memory_id, count, "auto-linked entities");
                }
                count as usize
            }
            Err(e) => {
                warn!(id = %memory_id, "entity matching failed: {e}");
                0
            }
        };

        // Embedding-based similarity linking with pre-computed vector
        let similar_hits = match auto_link_similar(
            &db,
            &memory_id,
            memory_project.as_deref(),
            vector,
        )
        .await
        {
            Ok(count) => {
                if count > 0 {
                    tracing::info!(id = %memory_id, count, "auto-linked similar memories");
                }
                count as usize
            }
            Err(e) => {
                warn!(id = %memory_id, "auto-link similar failed: {e}");
                0
            }
        };

        tracing::debug!(
            id = %memory_id,
            entity_matches,
            similar_hits,
            "auto-link complete"
        );
    }).await;
}
