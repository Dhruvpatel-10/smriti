//! High-level LLM relation extraction: SidecarClient → validation → persistence.
//!
//! Called from the post-create async pipeline (P1-E will wire this into
//! `kb_remember`). This module never panics; failures are logged and swallowed
//! because the pipeline is best-effort.

use std::sync::Arc;
use tracing::{info, instrument, warn};

use crate::db::{client::SurrealClient, memory as db_memory, typed_relation};
use crate::error::Result;
use crate::llm::sidecar::{ExtractAnchorsParams, ExtractRelationsParams, SidecarClient};
use crate::types::{MemoryId, RelationKind};

/// Extractor version embedded in every persisted edge — bump when prompt /
/// schema / model materially changes so callers can re-extract.
pub const EXTRACTOR_VERSION: &str = "claude-sonnet-4-5/v1";

/// Run relation extraction for a single memory and persist results.
///
/// Never panics. All sidecar / persistence errors are logged and counted; the
/// caller receives only the aggregate stats. This keeps the post-create async
/// pipeline resilient to LLM transients.
#[allow(clippy::too_many_arguments)]
#[instrument(skip(db, sidecar, summary, content))]
pub async fn extract_and_persist(
    db: &SurrealClient,
    sidecar: &Arc<SidecarClient>,
    memory_id: &MemoryId,
    summary: &str,
    content: Option<&str>,
    project: Option<&str>,
    domain: Option<&str>,
    confidence_threshold: f32,
) -> Result<ExtractStats> {
    let params = ExtractRelationsParams {
        memory_id: memory_id.as_str().to_string(),
        summary: summary.to_string(),
        content: content.map(|s| s.to_string()),
        project: project.map(|s| s.to_string()),
        domain: domain.map(|s| s.to_string()),
    };

    let relations = match sidecar.extract_relations(params).await {
        Ok(r) => r,
        Err(e) => {
            warn!(target: "extractor", "sidecar call failed: {e}");
            return Ok(ExtractStats::default());
        }
    };

    let mut stats = ExtractStats {
        returned: relations.len(),
        ..Default::default()
    };

    for relation in relations {
        // Guard: sidecar must report `from` matching the memory we submitted.
        if relation.from != memory_id.as_str() {
            stats.bad_from += 1;
            continue;
        }

        if relation.confidence < confidence_threshold {
            stats.below_threshold += 1;
            continue;
        }

        let kind: RelationKind = match relation.kind.parse() {
            Ok(k) => k,
            Err(_) => {
                stats.bad_kind += 1;
                continue;
            }
        };

        // The sidecar may return IDs with the "memory:" table prefix; strip it
        // so the MemoryId newtype holds only the record key.
        let to_raw = relation
            .to
            .strip_prefix("memory:")
            .unwrap_or(&relation.to)
            .to_string();
        let to_id = MemoryId::from(to_raw);

        match typed_relation::upsert(
            db,
            memory_id,
            &to_id,
            kind,
            relation.confidence,
            EXTRACTOR_VERSION,
            memory_id,
        )
        .await
        {
            Ok(true) => stats.persisted += 1,
            Ok(false) => {
                // upsert returns false for both duplicate and target_missing;
                // the persistence layer already warns on target_missing.
                stats.duplicate += 1;
            }
            Err(e) => {
                warn!(target: "extractor", from = %memory_id, to = %to_id, "persist failed: {e}");
                // continue — best-effort
            }
        }
    }

    info!(
        target: "extractor",
        persisted = stats.persisted,
        returned = stats.returned,
        memory_id = %memory_id,
        "relation extraction complete"
    );

    Ok(stats)
}

/// Extract anchor entities for a single memory and persist them.
///
/// Best-effort: sidecar errors and DB errors are logged and swallowed so the
/// post-create pipeline stays resilient. Returns the count of anchors stored,
/// or 0 on any error or empty extraction.
#[instrument(skip(db, sidecar, summary))]
pub async fn extract_anchors_and_persist(
    db: &SurrealClient,
    sidecar: &Arc<SidecarClient>,
    memory_id: &MemoryId,
    summary: &str,
    project: Option<&str>,
    domain: Option<&str>,
) -> Result<usize> {
    let params = ExtractAnchorsParams {
        memory_id: memory_id.as_str().to_string(),
        summary: summary.to_string(),
        project: project.map(|s| s.to_string()),
        domain: domain.map(|s| s.to_string()),
    };

    let raw = match sidecar.extract_anchors(params).await {
        Ok(r) => r,
        Err(e) => {
            warn!(target: "extractor", memory_id = %memory_id, "anchor sidecar call failed: {e}");
            return Ok(0);
        }
    };

    // Trim, filter empty, clamp to 3 (sidecar already clamps, but defence-in-depth).
    let anchors: Vec<String> = clean_anchors(raw);

    if anchors.is_empty() {
        return Ok(0);
    }

    match db_memory::set_anchors(db, memory_id, &anchors).await {
        Ok(()) => {
            info!(
                target: "extractor",
                memory_id = %memory_id,
                count = anchors.len(),
                "anchor entities persisted"
            );
            Ok(anchors.len())
        }
        Err(e) => {
            warn!(target: "extractor", memory_id = %memory_id, "anchor persist failed: {e}");
            Ok(0)
        }
    }
}

/// Trim, filter empty, clamp to 3 — pure function used in tests.
pub(crate) fn clean_anchors(raw: Vec<String>) -> Vec<String> {
    raw.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(3)
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::clean_anchors;

    #[test]
    fn clean_anchors_trims_and_filters_empty() {
        let raw = vec![
            "  Sarah  ".to_string(),
            "".to_string(),
            "gRPC interceptor rollout".to_string(),
            "   ".to_string(),
            "Rust".to_string(),
        ];
        let result = clean_anchors(raw);
        assert_eq!(result, vec!["Sarah", "gRPC interceptor rollout", "Rust"]);
    }

    #[test]
    fn clean_anchors_clamps_to_three() {
        let raw = vec![
            "Alice".to_string(),
            "Bob".to_string(),
            "Carol".to_string(),
            "Dave".to_string(),
        ];
        let result = clean_anchors(raw);
        assert_eq!(result.len(), 3);
        assert_eq!(result, vec!["Alice", "Bob", "Carol"]);
    }

    #[test]
    fn clean_anchors_empty_input_returns_empty() {
        let result = clean_anchors(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn clean_anchors_all_whitespace_returns_empty() {
        let raw = vec!["   ".to_string(), "\t".to_string()];
        let result = clean_anchors(raw);
        assert!(result.is_empty());
    }
}

/// Aggregate counters from a single extraction run.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExtractStats {
    /// Total relations returned by the sidecar.
    pub returned: usize,
    /// New edges created in DB.
    pub persisted: usize,
    /// Relations dropped because confidence was below threshold.
    pub below_threshold: usize,
    /// Relations dropped because kind was not a known `RelationKind` variant.
    pub bad_kind: usize,
    /// Relations dropped because `from` didn't match the submitted memory ID.
    pub bad_from: usize,
    /// Relations not persisted because the target memory didn't exist or the
    /// edge already existed.
    pub duplicate: usize,
}
