use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use tracing::{info, instrument, warn};

use crate::config::EmbeddingConfig;
use crate::db::embeddings::fingerprint::EmbedderFingerprint;
use crate::db::embeddings::traits::Embedder;
use crate::error::{KbError, Result};

/// In-process embedding service using ONNX Runtime (via fastembed).
///
/// Default model: `google/embeddinggemma-300m` — SOTA under 500M params,
/// MRL-truncated to 256 dims for 3x storage savings with minimal quality loss.
///
/// The model is loaded lazily on the first `embed()` call. Subsequent calls
/// reuse the loaded model. Thread-safe via internal Mutex (fastembed's
/// `embed()` takes `&mut self`).
///
/// No external service required — the ONNX model is downloaded once on first
/// use (~200MB) and cached in the fastembed cache directory.
#[derive(Clone)]
pub struct LocalEmbedder {
    model: Arc<OnceLock<std::result::Result<Mutex<TextEmbedding>, String>>>,
    model_id: EmbeddingModel,
    pub(crate) dimensions: usize,
}

impl LocalEmbedder {
    pub fn new(config: &EmbeddingConfig) -> Self {
        let model_id = match config.model.as_str() {
            "embeddinggemma-300m" | "gemma" => EmbeddingModel::EmbeddingGemma300M,
            "nomic-embed-text-v1.5" | "nomic" => EmbeddingModel::NomicEmbedTextV15,
            "bge-small-en-v1.5" | "bge-small" => EmbeddingModel::BGESmallENV15,
            "bge-large-en-v1.5" | "bge-large" => EmbeddingModel::BGELargeENV15,
            "gte-large-en-v1.5-q" | "gte-q" => EmbeddingModel::GTELargeENV15Q,
            "all-minilm-l6-v2" | "minilm" => EmbeddingModel::AllMiniLML6V2,
            "jina-v2-base-en" | "jina" => EmbeddingModel::JinaEmbeddingsV2BaseEN,
            "gte-large-en-v1.5" | "gte" => EmbeddingModel::GTELargeENV15,
            "mxbai-embed-large-v1" | "mxbai" => EmbeddingModel::MxbaiEmbedLargeV1,
            other => {
                warn!(
                    model = other,
                    "unknown embedding model, falling back to EmbeddingGemma-300M"
                );
                EmbeddingModel::EmbeddingGemma300M
            }
        };

        Self {
            model: Arc::new(OnceLock::new()),
            model_id,
            dimensions: config.dimensions,
        }
    }

    pub fn model_name(&self) -> &str {
        match self.model_id {
            EmbeddingModel::EmbeddingGemma300M => "embeddinggemma-300m",
            EmbeddingModel::NomicEmbedTextV15 => "nomic-embed-text-v1.5",
            EmbeddingModel::BGESmallENV15 => "bge-small-en-v1.5",
            EmbeddingModel::BGELargeENV15 => "bge-large-en-v1.5",
            EmbeddingModel::GTELargeENV15Q => "gte-large-en-v1.5-q",
            EmbeddingModel::AllMiniLML6V2 => "all-minilm-l6-v2",
            EmbeddingModel::JinaEmbeddingsV2BaseEN => "jina-v2-base-en",
            EmbeddingModel::GTELargeENV15 => "gte-large-en-v1.5",
            EmbeddingModel::MxbaiEmbedLargeV1 => "mxbai-embed-large-v1",
            _ => "unknown",
        }
    }

    /// Ensure the model is loaded. Called lazily on first embed().
    fn ensure_loaded(&self) -> Result<()> {
        let result = self.model.get_or_init(|| {
            info!("loading embedding model (first use — may download ~200MB)");
            let opts = InitOptions::new(self.model_id.clone()).with_show_download_progress(true);
            match TextEmbedding::try_new(opts) {
                Ok(model) => {
                    info!("embedding model loaded");
                    Ok(Mutex::new(model))
                }
                Err(e) => {
                    let msg = format!("failed to load embedding model: {e}");
                    // WARN not ERROR — server continues without vectors (graceful degrade)
                    tracing::warn!("{msg}");
                    Err(msg)
                }
            }
        });
        match result {
            Ok(_) => Ok(()),
            Err(msg) => Err(KbError::Embedding(msg.clone())),
        }
    }

    /// Generate an embedding vector for the given text.
    ///
    /// First call triggers model download/load (~2-10s). Subsequent calls
    /// run ONNX inference in-process (~10-50ms per text on CPU).
    ///
    /// Output is MRL-truncated to `self.dimensions` (default 256).
    #[instrument(skip(self, text))]
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.ensure_loaded()?;
        let model_arc = self.model.clone();
        let text = text.to_string();
        let dims = self.dimensions;

        tokio::task::spawn_blocking(move || {
            let loaded = model_arc
                .get()
                .ok_or_else(|| KbError::Embedding("embedding model not initialized".to_string()))?;
            let mutex = loaded
                .as_ref()
                .map_err(|msg| KbError::Embedding(msg.clone()))?;
            let mut guard = mutex
                .lock()
                .map_err(|e| KbError::Embedding(format!("model lock poisoned: {e}")))?;
            let results = guard
                .embed(vec![text.as_str()], None)
                .map_err(|e| KbError::Embedding(format!("embedding failed: {e}")))?;
            let mut vector = results
                .into_iter()
                .next()
                .ok_or_else(|| KbError::Embedding("empty embedding result".to_string()))?;

            // MRL truncation: Matryoshka-trained models retain quality at lower dims.
            // 256 dims ≈ 97% of full 768 quality, 3x less storage.
            if dims > 0 && vector.len() > dims {
                vector.truncate(dims);
            }

            Ok(vector)
        })
        .await
        .map_err(|e| KbError::Embedding(format!("blocking task failed: {e}")))?
    }
}

#[async_trait]
impl Embedder for LocalEmbedder {
    fn fingerprint(&self) -> EmbedderFingerprint {
        EmbedderFingerprint {
            provider: "local".to_string(),
            model: self.model_name().to_string(),
            dimensions: self.dimensions,
        }
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn max_input_tokens(&self) -> usize {
        512
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text).await
    }

    async fn embed_document(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text).await
    }
}
