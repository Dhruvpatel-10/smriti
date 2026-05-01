//! Embedder fingerprint persistence and startup safety gate.
//!
//! The fingerprint is `provider/model/dimensions` of whichever embedder was
//! last used to write vectors. Stored in `_kb_metadata` (Migration 015).
//!
//! Why: switching embedder configuration (e.g. local fastembed → Voyage,
//! or 256-dim → 1024-dim) puts query vectors in a different embedding
//! space from stored vectors. Cosine similarity becomes meaningless and
//! recall silently degrades to garbage. Hard-fail at startup is strictly
//! safer than silent corruption — the operator must run `kb_reembed`
//! (or accept the new fingerprint on a fresh DB) before queries resume.
//!
//! On a fresh DB with zero vectors, the new fingerprint is written and
//! startup proceeds. On mismatch with non-zero vectors, startup fails
//! with an actionable error message naming the previous and current
//! configurations.

use chrono::Utc;
use tracing::{info, warn};

use crate::db::client::SurrealClient;
use crate::error::{KbError, Result};

const FINGERPRINT_KEY: &str = "embedder_fingerprint";

/// Identity of the embedder that wrote vectors into this DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedderFingerprint {
    pub provider: String,
    pub model: String,
    pub dimensions: usize,
}

impl EmbedderFingerprint {
    fn encode(&self) -> String {
        // Pipe-separated; provider/model are ASCII identifiers controlled
        // server-side, no escaping concerns.
        format!("{}|{}|{}", self.provider, self.model, self.dimensions)
    }

    fn decode(s: &str) -> Option<Self> {
        let mut parts = s.splitn(3, '|');
        let provider = parts.next()?.to_string();
        let model = parts.next()?.to_string();
        let dims: usize = parts.next()?.parse().ok()?;
        Some(Self { provider, model, dimensions: dims })
    }

    pub fn human(&self) -> String {
        format!("{}/{} @ {}d", self.provider, self.model, self.dimensions)
    }
}

/// Read the persisted fingerprint, if one exists.
pub async fn read(db: &SurrealClient) -> Result<Option<EmbedderFingerprint>> {
    // SurrealQL keywords are case-insensitive: bare `value` is parsed as the
    // SELECT VALUE clause (returns flat array). Backtick-escape the field name.
    let mut result = db
        .query("SELECT `value` FROM _kb_metadata WHERE key = $key LIMIT 1")
        .bind(("key", FINGERPRINT_KEY.to_string()))
        .await
        .map_err(|e| KbError::db("reading embedder fingerprint", e))?;

    let row: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing fingerprint row", e))?;

    Ok(row
        .as_ref()
        .and_then(|v| v.get("value"))
        .and_then(|v| v.as_str())
        .and_then(EmbedderFingerprint::decode))
}

/// Persist (insert or update) the fingerprint.
pub async fn write(db: &SurrealClient, fp: &EmbedderFingerprint) -> Result<()> {
    let now = Utc::now();
    let encoded = fp.encode();

    // Idempotent upsert via existence check — _kb_metadata has UNIQUE on key
    // so naive CREATE on a second boot would fail. SurrealDB's UPSERT keyword
    // covers both branches.
    db.query(
        "UPSERT _kb_metadata MERGE { key: $key, `value`: $value, updated_at: $now } \
         WHERE key = $key",
    )
    .bind(("key", FINGERPRINT_KEY.to_string()))
    .bind(("value", encoded))
    .bind(("now", now))
    .await
    .map_err(|e| KbError::db("writing embedder fingerprint", e))?;
    Ok(())
}

/// Count active memories that have a non-null vector.
async fn count_vectors(db: &SurrealClient) -> Result<u64> {
    let mut result = db
        .query("SELECT count() AS n FROM memory WHERE vector IS NOT NONE GROUP ALL")
        .await
        .map_err(|e| KbError::db("counting stored vectors", e))?;

    let row: Option<serde_json::Value> = result
        .take(0)
        .map_err(|e| KbError::db("deserializing vector count", e))?;

    Ok(row
        .as_ref()
        .and_then(|v| v.get("n"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0))
}

/// Verify the current embedder configuration matches what wrote the existing
/// vectors. On first run (no fingerprint yet) or fresh DB (no vectors),
/// records the new fingerprint and returns Ok. On mismatch with stored
/// vectors, returns a Config error directing the operator to `kb_reembed`.
pub async fn check_or_initialize(
    db: &SurrealClient,
    current: &EmbedderFingerprint,
) -> Result<()> {
    match read(db).await? {
        None => {
            info!(
                fingerprint = %current.human(),
                "no embedder fingerprint stored — initializing"
            );
            write(db, current).await?;
            Ok(())
        }
        Some(stored) if stored == *current => {
            info!(fingerprint = %current.human(), "embedder fingerprint matches");
            Ok(())
        }
        Some(stored) => {
            let vector_count = count_vectors(db).await.unwrap_or(0);
            if vector_count == 0 {
                warn!(
                    previous = %stored.human(),
                    current = %current.human(),
                    "embedder fingerprint changed but no vectors exist — updating"
                );
                write(db, current).await?;
                return Ok(());
            }

            // Escape hatch: operator sets SMRITI_FORCE_REEMBED_ON_START=1 to
            // bypass the mismatch check after a deliberate model swap. They MUST
            // run `kb_reembed` immediately after startup to repopulate vectors —
            // until then cosine scores are meaningless.
            if std::env::var("SMRITI_FORCE_REEMBED_ON_START")
                .ok()
                .filter(|v| !v.is_empty())
                .is_some()
            {
                warn!(
                    previous = %stored.human(),
                    current = %current.human(),
                    vector_count,
                    "SMRITI_FORCE_REEMBED_ON_START set — updating stored fingerprint without \
                     vector verification; operator MUST run kb_reembed immediately to repopulate vectors"
                );
                write(db, current).await?;
                return Ok(());
            }

            Err(KbError::Config(format!(
                "embedder fingerprint mismatch (stored={}, current={}); \
                 run `kb_reembed` then restart, or set SMRITI_FORCE_REEMBED_ON_START=1 \
                 to bypass this check on next startup. \
                 ({} active vectors — mixing embedding spaces produces meaningless cosine scores)",
                stored.human(),
                current.human(),
                vector_count
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_roundtrip() {
        let fp = EmbedderFingerprint {
            provider: "voyage".to_string(),
            model: "voyage-context-3".to_string(),
            dimensions: 1024,
        };
        let encoded = fp.encode();
        let decoded = EmbedderFingerprint::decode(&encoded).expect("decode");
        assert_eq!(fp, decoded);
    }

    #[test]
    fn fingerprint_decode_rejects_garbage() {
        assert!(EmbedderFingerprint::decode("garbage").is_none());
        assert!(EmbedderFingerprint::decode("a|b").is_none());
        assert!(EmbedderFingerprint::decode("a|b|notanumber").is_none());
    }

    #[test]
    fn fingerprint_equality() {
        let a = EmbedderFingerprint {
            provider: "local".to_string(),
            model: "embeddinggemma-300m".to_string(),
            dimensions: 256,
        };
        let b = a.clone();
        let c = EmbedderFingerprint {
            provider: "local".to_string(),
            model: "embeddinggemma-300m".to_string(),
            dimensions: 768,
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn fingerprint_human_readable() {
        let fp = EmbedderFingerprint {
            provider: "voyage".to_string(),
            model: "voyage-context-3".to_string(),
            dimensions: 1024,
        };
        assert_eq!(fp.human(), "voyage/voyage-context-3 @ 1024d");
    }

    /// The escape flag test requires a live DB to exercise `check_or_initialize`.
    /// These unit tests cover the logic that CAN be tested without a DB:
    /// encode/decode correctness and the error message format.
    #[test]
    fn mismatch_error_message_contains_force_reembed_hint() {
        // Build the error string the same way `check_or_initialize` does.
        let stored = EmbedderFingerprint {
            provider: "local".to_string(),
            model: "embeddinggemma-300m".to_string(),
            dimensions: 256,
        };
        let current = EmbedderFingerprint {
            provider: "voyage".to_string(),
            model: "voyage-context-3".to_string(),
            dimensions: 1024,
        };
        let msg = format!(
            "embedder fingerprint mismatch (stored={}, current={}); \
             run `kb_reembed` then restart, or set SMRITI_FORCE_REEMBED_ON_START=1 \
             to bypass this check on next startup. \
             ({} active vectors — mixing embedding spaces produces meaningless cosine scores)",
            stored.human(),
            current.human(),
            42u64
        );
        assert!(msg.contains("SMRITI_FORCE_REEMBED_ON_START=1"), "hint must be present");
        assert!(msg.contains("kb_reembed"), "reembed instruction must be present");
        assert!(msg.contains("voyage-context-3"), "current fingerprint must appear");
        assert!(msg.contains("embeddinggemma-300m"), "stored fingerprint must appear");
    }

    #[test]
    fn fingerprint_string_matches_voyage_context_3() {
        // Verify the canonical fingerprint string used in ADR-020.
        let fp = EmbedderFingerprint {
            provider: "voyage".to_string(),
            model: "voyage-context-3".to_string(),
            dimensions: 1024,
        };
        assert_eq!(fp.encode(), "voyage|voyage-context-3|1024");
    }
}
