use serde::Deserialize;
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
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            overdue_action_item_days: 1,
            stale_contact_days: 14,
            expiring_memory_hours: 24,
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

#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingConfig {
    /// Enable vector embeddings (loads ONNX model in-process on first use).
    pub enabled: bool,
    /// Model name. Supported: "embeddinggemma-300m" (default, SOTA), "nomic-embed-text-v1.5",
    /// "bge-small-en-v1.5", "all-minilm-l6-v2", and 40+ more via fastembed.
    pub model: String,
    /// Output dimensions after MRL truncation. 0 = use model's native dims.
    /// EmbeddingGemma-300M supports MRL: 768 (full), 256 (recommended), 128.
    pub dimensions: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: "embeddinggemma-300m".to_string(),
            dimensions: 256,
        }
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
        if let Some(rest) = path.strip_prefix("~/").filter(|_| std::env::var("HOME").is_ok()) {
            return std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(rest);
        }
        std::path::PathBuf::from(path)
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

        cfg.try_deserialize().map_err(|e| KbError::Config(e.to_string()))
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

    /// Resolve queue dir from env or default.
    pub fn queue_dir(&self) -> PathBuf {
        std::env::var("KB_QUEUE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("queue"))
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
}
