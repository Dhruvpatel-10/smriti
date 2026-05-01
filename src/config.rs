use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{KbError, Result};

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub database: DatabaseConfig,
    pub pruning: PruningConfig,
    pub queue: QueueConfig,
    #[serde(default)]
    pub alerts: AlertConfig,
    #[serde(default)]
    pub export: ExportConfig,
    #[serde(default)]
    pub embeddings: EmbeddingConfig,
    #[serde(default)]
    pub markdown: MarkdownConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub consolidation: ConsolidationConfig,
    #[serde(default)]
    pub server: ServerConfig,
}

/// HTTP server configuration. Nested under `[server]` in config.toml.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ServerConfig {
    #[serde(default)]
    pub auth: AuthConfig,
}

/// Bearer-token auth for the HTTP daemon (ADR-027).
///
/// `tokens_file` points to a TOML file with structure:
/// ```toml
/// [tokens]
/// alice = "tok_<random-32-bytes-hex>"
/// bob   = "tok_<random-32-bytes-hex>"
/// ```
/// If the file is missing or empty the server logs a warning and accepts all
/// requests — useful for single-user setups.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    /// Path to the TOML file containing per-user bearer tokens.
    /// Default: `/etc/smriti/tokens.toml`.
    #[serde(default = "default_tokens_file")]
    pub tokens_file: PathBuf,
}

fn default_tokens_file() -> PathBuf {
    PathBuf::from("/etc/smriti/tokens.toml")
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            tokens_file: default_tokens_file(),
        }
    }
}

/// Per-edge-type weight overrides for spreading activation.
/// All fields default to the ADR-023 calibrated starting values.
#[derive(Debug, Clone, Deserialize)]
pub struct EdgeWeightsConfig {
    /// Causal provenance edges (sidecar-extracted). Full propagation.
    #[serde(default = "default_weight_derives")]
    pub derives: f64,
    /// Intentional structural links (kb_link). Strong.
    #[serde(default = "default_weight_extends")]
    pub extends: f64,
    /// Explicit version-chain supersede links. Strong.
    #[serde(default = "default_weight_updates")]
    pub updates: f64,
    /// General declared links. Medium.
    #[serde(default = "default_weight_relates")]
    pub relates: f64,
    /// Entity co-occurrence links (auto-linked at ≥0.75 cosine). Weak —
    /// prevents super-hub entity nodes from dominating spreading activation.
    #[serde(default = "default_weight_involves")]
    pub involves: f64,
    /// Multiplied by the sidecar-supplied confidence ∈ [0,1]. High-confidence
    /// extractions approach `derives`; low-confidence approach `relates`.
    #[serde(default = "default_weight_typed_relation_max")]
    pub typed_relation_max: f64,
}

fn default_weight_derives() -> f64 { 1.0 }
fn default_weight_extends() -> f64 { 0.8 }
fn default_weight_updates() -> f64 { 0.8 }
fn default_weight_relates() -> f64 { 0.5 }
fn default_weight_involves() -> f64 { 0.3 }
fn default_weight_typed_relation_max() -> f64 { 0.9 }

impl Default for EdgeWeightsConfig {
    fn default() -> Self {
        Self {
            derives: default_weight_derives(),
            extends: default_weight_extends(),
            updates: default_weight_updates(),
            relates: default_weight_relates(),
            involves: default_weight_involves(),
            typed_relation_max: default_weight_typed_relation_max(),
        }
    }
}

/// Configuration for the spreading-activation stage of recall (ADR-023).
/// Nested under `[search.spreading]` in config.toml.
#[derive(Debug, Clone, Deserialize)]
pub struct SpreadingConfig {
    /// Maximum number of spreading-activation neighbors added to the result set.
    /// Default 3 — was previously hardcoded.
    #[serde(default = "default_spreading_cap")]
    pub cap: usize,
    /// Score decay per hop. At depth d: `parent_score × edge_weight × hop_decay^d`.
    /// Default 0.5 — halves contribution at each hop.
    #[serde(default = "default_hop_decay")]
    pub hop_decay: f64,
    /// Per-edge-type weight coefficients. See ADR-023.
    #[serde(default)]
    pub edge_weights: EdgeWeightsConfig,
}

fn default_spreading_cap() -> usize { 3 }
fn default_hop_decay() -> f64 { 0.5 }

impl Default for SpreadingConfig {
    fn default() -> Self {
        Self {
            cap: default_spreading_cap(),
            hop_decay: default_hop_decay(),
            edge_weights: EdgeWeightsConfig::default(),
        }
    }
}

/// Fusion strategy for combining BM25 and vector result lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FusionStrategy {
    /// Reciprocal Rank Fusion — rank-position-only, robust to uncalibrated scores.
    #[default]
    Rrf,
    /// Convex Combination — weighted sum of min-max-normalised scores.
    /// Preserves score magnitude; outperforms RRF when distributions are calibrated.
    ConvexCombination,
}

/// Configuration for the fusion stage that combines BM25 and vector results.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FusionConfig {
    /// Which algorithm merges BM25 and vector lists. Default: rrf.
    #[serde(default)]
    pub strategy: FusionStrategy,
    /// Weight given to the vector score in convex combination.
    /// BM25 weight = 1.0 - vector_weight. Range [0.0, 1.0]. Default 0.7.
    #[serde(default = "default_vector_weight")]
    pub vector_weight: f64,
    /// Items whose fused CC score is below this floor are dropped.
    /// Default 0.0 (no floor). Only applied for ConvexCombination strategy;
    /// RRF uses its own min_score passed from the caller.
    #[serde(default)]
    pub min_score: f64,
}

fn default_vector_weight() -> f64 {
    0.7
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self {
            strategy: FusionStrategy::default(),
            vector_weight: default_vector_weight(),
            min_score: 0.0,
        }
    }
}

/// Top-level search/retrieval configuration. Nested under `[search]`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchConfig {
    #[serde(default)]
    pub spreading: SpreadingConfig,
    #[serde(default)]
    pub fusion: FusionConfig,
}

#[derive(Debug, Deserialize)]
pub struct DatabaseConfig {
    pub path: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PruningConfig {
    pub working_scratch_ttl_hours: u64,
    pub working_priority_ttl_days: u64,
    pub working_open_question_ttl_days: u64,
    pub access_decay_interval_days: u64,
    pub consolidation_interval_hours: u64,
    pub importance_archive_threshold: u8,
}

/// Configuration for auto-consolidation via the LLM sidecar.
/// Nested under `[consolidation]` in config.toml.
#[derive(Debug, Clone, Deserialize)]
pub struct ConsolidationConfig {
    /// Enable automatic LLM-driven consolidation on each periodic tick. Default false.
    #[serde(default)]
    pub auto_enabled: bool,
    /// Only consider memories at least this many days old. Default 7.
    #[serde(default = "default_consolidation_min_age_days")]
    pub min_age_days: u32,
    /// Skip merge when sidecar confidence is below this floor. Default 0.75.
    #[serde(default = "default_consolidation_min_confidence")]
    pub min_confidence: f32,
    /// Maximum clusters processed per tick. Default 5.
    #[serde(default = "default_consolidation_max_per_tick")]
    pub max_per_tick: u32,
    /// Maximum summaries per cluster sent to the sidecar. Default 12.
    #[serde(default = "default_consolidation_max_cluster_size")]
    pub max_cluster_size: u32,
    /// Days to wait before re-evaluating a cluster that was previously skipped. Default 7.
    #[serde(default = "default_consolidation_cooldown_days")]
    pub cooldown_days: u32,
    /// Hard cap on total sidecar calls across all ticks in a rolling 24h window.
    /// Prevents runaway spend when many new clusters appear. Default 20.
    #[serde(default = "default_consolidation_max_attempts_per_day")]
    pub max_attempts_per_day: u32,
}

fn default_consolidation_min_age_days() -> u32 { 7 }
fn default_consolidation_min_confidence() -> f32 { 0.75 }
fn default_consolidation_max_per_tick() -> u32 { 5 }
fn default_consolidation_max_cluster_size() -> u32 { 12 }
fn default_consolidation_cooldown_days() -> u32 { 7 }
fn default_consolidation_max_attempts_per_day() -> u32 { 20 }

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            auto_enabled: false,
            min_age_days: default_consolidation_min_age_days(),
            min_confidence: default_consolidation_min_confidence(),
            max_per_tick: default_consolidation_max_per_tick(),
            max_cluster_size: default_consolidation_max_cluster_size(),
            cooldown_days: default_consolidation_cooldown_days(),
            max_attempts_per_day: default_consolidation_max_attempts_per_day(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct QueueConfig {
    pub drain_interval_secs: u64,
    pub max_queue_size: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlertConfig {
    /// Days after event_at before an action item is considered overdue.
    pub overdue_action_item_days: u64,
    /// Days since last contact before an entity is considered stale.
    pub stale_contact_days: u64,
    /// Hours before expiry to flag a memory as worth promoting.
    pub expiring_memory_hours: u64,
    /// Number of memories with failed extraction status that triggers an
    /// ExtractionBacklog alert. Default 50.
    #[serde(default = "default_extraction_backlog_threshold")]
    pub extraction_backlog_threshold: u64,
}

fn default_extraction_backlog_threshold() -> u64 {
    50
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            overdue_action_item_days: 1,
            stale_contact_days: 14,
            expiring_memory_hours: 24,
            extraction_backlog_threshold: default_extraction_backlog_threshold(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExportConfig {
    /// Directory for backup exports (relative to KB root or absolute).
    pub backup_dir: String,
    /// Exclude sensitive-visibility memories from export by default.
    pub exclude_sensitive: bool,
}

impl Default for ExportConfig {
    fn default() -> Self {
        Self {
            backup_dir: "archive/exports".to_string(),
            exclude_sensitive: true,
        }
    }
}

/// Which embedding backend to use. Default is `Local` (fastembed, no network).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingProvider {
    /// In-process ONNX via fastembed — no network dependency.
    #[default]
    Local,
    /// Voyage AI cloud embedder — requires API key.
    Voyage,
}

/// Voyage AI-specific configuration. Only required when `provider = "voyage"`.
#[derive(Clone, Deserialize)]
pub struct VoyageConfig {
    /// API key. If `None`, falls back to `VOYAGE_API_KEY` env var.
    pub api_key: Option<String>,
    /// Model name. Default: "voyage-context-3".
    #[serde(default = "default_voyage_model")]
    pub model: String,
    /// Output dimension requested from the API. Default: 1024.
    #[serde(default = "default_voyage_output_dimension")]
    pub output_dimension: usize,
    /// HTTP request timeout in milliseconds. Default: 30000.
    #[serde(default = "default_voyage_timeout")]
    pub request_timeout_ms: u64,
    /// Privacy acknowledgment. Voyage's no-training guarantee requires a
    /// payment method on file (paid tier semantics, regardless of free quota
    /// usage). Without `accepted_terms = true`, smriti will refuse to start
    /// with `provider = "voyage"`. See ADR-016.
    #[serde(default)]
    pub accepted_terms: bool,
    /// Cross-encoder reranker configuration (ADR-017).
    #[serde(default)]
    pub reranker: VoyageRerankerConfig,
}

fn default_voyage_model() -> String {
    "voyage-context-3".to_string()
}
fn default_voyage_output_dimension() -> usize {
    1024
}
fn default_voyage_timeout() -> u64 {
    30000
}

/// Voyage AI reranker configuration. Nested under `[embeddings.voyage.reranker]`.
#[derive(Clone, Debug, Deserialize, Default)]
pub struct VoyageRerankerConfig {
    /// Enable the cross-encoder reranker (stage 3.5). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Reranker model name. Default: "rerank-2.5-lite".
    #[serde(default = "default_reranker_model")]
    pub model: String,
    /// Maximum number of candidates sent to the reranker per call. Default: 50.
    #[serde(default = "default_reranker_top_k")]
    pub top_k_input: usize,
    /// Per-query gating policy (ADR-022). Default: Adaptive.
    #[serde(default)]
    pub policy: crate::types::RerankerPolicy,
}

fn default_reranker_model() -> String {
    "rerank-2.5-lite".to_string()
}
fn default_reranker_top_k() -> usize {
    50
}

impl Default for VoyageConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            model: default_voyage_model(),
            output_dimension: default_voyage_output_dimension(),
            request_timeout_ms: default_voyage_timeout(),
            accepted_terms: false,
            reranker: VoyageRerankerConfig::default(),
        }
    }
}

// api_key is a secret — never print its value in logs or debug output.
impl std::fmt::Debug for VoyageConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoyageConfig")
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("model", &self.model)
            .field("output_dimension", &self.output_dimension)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("accepted_terms", &self.accepted_terms)
            .field("reranker", &self.reranker)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingConfig {
    /// Enable vector embeddings (loads ONNX model in-process on first use).
    pub enabled: bool,
    /// Backend provider: "local" (default, fastembed) or "voyage" (cloud).
    #[serde(default)]
    pub provider: EmbeddingProvider,
    /// Model name. Supported: "embeddinggemma-300m" (default, SOTA), "gte-large-en-v1.5-q"
    /// (1024-dim int8 quantized, recommended for bench), "bge-small-en-v1.5",
    /// "all-minilm-l6-v2", and 40+ more via fastembed.
    pub model: String,
    /// Output dimensions after MRL truncation. 0 = use model's native dims.
    /// EmbeddingGemma-300M supports MRL: 768 (full), 256 (recommended), 128.
    pub dimensions: usize,
    /// Voyage AI configuration. Only required when `provider = "voyage"`.
    pub voyage: Option<VoyageConfig>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: EmbeddingProvider::Local,
            model: "embeddinggemma-300m".to_string(),
            dimensions: 256,
            voyage: None,
        }
    }
}

impl EmbeddingConfig {
    /// Resolve the Voyage API key from config or env var.
    ///
    /// Returns an error with an actionable message if provider=Voyage and no
    /// key is available — surfaced at startup, not on the first embed call.
    pub fn resolve_voyage_key(&self) -> crate::error::Result<String> {
        let vcfg = self.voyage.as_ref();
        let from_config = vcfg.and_then(|v| v.api_key.clone());
        from_config
            .or_else(|| std::env::var("VOYAGE_API_KEY").ok())
            .ok_or_else(|| {
                crate::error::KbError::Config(
                    "Voyage AI provider selected but no API key found. \
                     Set embeddings.voyage.api_key in config.toml or the \
                     VOYAGE_API_KEY environment variable."
                        .to_string(),
                )
            })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarkdownConfig {
    /// Enable automatic markdown layer regeneration.
    pub enabled: bool,
    /// Output path for the generated CLAUDE.md file.
    /// Supports `~` for home directory.
    pub output_path: String,
}

impl Default for MarkdownConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            output_path: "~/.claude/CLAUDE.md".to_string(),
        }
    }
}

impl MarkdownConfig {
    /// Resolve the output path, expanding `~` to the user's home directory.
    pub fn resolved_path(&self) -> std::path::PathBuf {
        let path = &self.output_path;
        if let Some(rest) = path.strip_prefix("~/")
            && let Ok(home) = std::env::var("HOME")
        {
            return std::path::PathBuf::from(home).join(rest);
        }
        std::path::PathBuf::from(path)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LlmConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_sidecar_path")]
    pub sidecar_path: PathBuf,
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f32,
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
    #[serde(default = "default_max_consecutive_failures")]
    pub max_consecutive_failures: u32,
}

fn default_sidecar_path() -> PathBuf {
    PathBuf::from("sidecar/dist/index.js")
}
fn default_confidence_threshold() -> f32 {
    0.7
}
fn default_request_timeout_ms() -> u64 {
    30000
}
fn default_max_consecutive_failures() -> u32 {
    5
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sidecar_path: default_sidecar_path(),
            confidence_threshold: default_confidence_threshold(),
            request_timeout_ms: default_request_timeout_ms(),
            max_consecutive_failures: default_max_consecutive_failures(),
        }
    }
}

impl AppConfig {
    pub fn load(base_dir: &Path) -> Result<Self> {
        let config_path = base_dir.join("config.toml");
        let cfg = config::Config::builder()
            .add_source(config::File::from(config_path))
            .add_source(config::Environment::with_prefix("KB"))
            .build()
            .map_err(|e| KbError::Config(e.to_string()))?;

        let config: Self = cfg.try_deserialize().map_err(|e| KbError::Config(e.to_string()))?;

        let vw = config.search.fusion.vector_weight;
        if !(0.0..=1.0).contains(&vw) {
            return Err(KbError::Config(format!(
                "search.fusion.vector_weight must be in [0.0, 1.0], got {vw}"
            )));
        }

        tracing::info!(
            db_path = %config.db_path(base_dir).display(),
            export_dir = %config.export_dir(base_dir).display(),
            queue_dir = %config.queue_dir().ok().map(|p| p.display().to_string()).unwrap_or_else(|| "unset".into()),
            embeddings_enabled = %config.embeddings.enabled,
            "config loaded"
        );

        Ok(config)
    }

    /// Resolve database path relative to base_dir if not absolute.
    pub fn db_path(&self, base_dir: &Path) -> PathBuf {
        let p = Path::new(&self.database.path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            base_dir.join(p)
        }
    }

    /// Resolve queue dir from env or default, then create it if absent.
    ///
    /// Returns an error if `KB_QUEUE_DIR` is set to an invalid (non-UTF-8) value
    /// or if the directory cannot be created.
    pub fn queue_dir(&self) -> Result<PathBuf> {
        let dir = match std::env::var("KB_QUEUE_DIR") {
            Ok(val) if val.is_empty() => {
                return Err(KbError::Config(
                    "KB_QUEUE_DIR is set but empty".to_string(),
                ));
            }
            Ok(val) => PathBuf::from(val),
            Err(_) => PathBuf::from("queue"),
        };

        std::fs::create_dir_all(&dir).map_err(|e| {
            KbError::Config(format!(
                "cannot create queue directory '{}': {e}",
                dir.display()
            ))
        })?;

        // Guard: path must now be a directory (create_dir_all could succeed even
        // if a file already existed at a parent segment, but the leaf is a file).
        if !dir.is_dir() {
            return Err(KbError::Config(format!(
                "queue path '{}' exists but is not a directory",
                dir.display()
            )));
        }

        Ok(dir)
    }

    /// Resolve export backup dir relative to base_dir if not absolute.
    pub fn export_dir(&self, base_dir: &Path) -> PathBuf {
        let p = Path::new(&self.export.backup_dir);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            base_dir.join(p)
        }
    }

    /// Resolve sidecar path relative to base_dir if not absolute.
    pub fn resolved_sidecar_path(&self, base_dir: &Path) -> PathBuf {
        if self.llm.sidecar_path.is_absolute() {
            self.llm.sidecar_path.clone()
        } else {
            base_dir.join(&self.llm.sidecar_path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_config_default_enabled_false() {
        assert!(!LlmConfig::default().enabled);
    }

    #[test]
    fn llm_config_serde_round_trip() {
        let original = LlmConfig::default();
        let json = serde_json::to_string(&original).expect("serialize");
        let decoded: LlmConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.enabled, original.enabled);
        assert_eq!(decoded.sidecar_path, original.sidecar_path);
        assert!((decoded.confidence_threshold - original.confidence_threshold).abs() < f32::EPSILON);
        assert_eq!(decoded.request_timeout_ms, original.request_timeout_ms);
        assert_eq!(decoded.max_consecutive_failures, original.max_consecutive_failures);
    }

    #[test]
    fn llm_config_partial_json_uses_defaults() {
        // Serde fills missing fields from per-field defaults.
        let decoded: LlmConfig = serde_json::from_str("{}").expect("empty object deserialize");
        assert!(!decoded.enabled);
        assert_eq!(decoded.sidecar_path, PathBuf::from("sidecar/dist/index.js"));
        assert!((decoded.confidence_threshold - 0.7).abs() < f32::EPSILON);
        assert_eq!(decoded.request_timeout_ms, 30000);
        assert_eq!(decoded.max_consecutive_failures, 5);
    }
}
