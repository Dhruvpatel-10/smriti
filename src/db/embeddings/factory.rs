use std::sync::Arc;

use crate::config::{EmbeddingConfig, EmbeddingProvider};
use crate::db::embeddings::local::LocalEmbedder;
use crate::db::embeddings::traits::Embedder;
use crate::db::embeddings::voyage::VoyageEmbedder;
use crate::error::{KbError, Result};

/// Build the appropriate embedder backend from config.
///
/// For Voyage, validates the accepted_terms flag and resolves the API key.
/// For Local, constructs a `LocalEmbedder` with lazy model load.
pub fn build_embedder(cfg: &EmbeddingConfig) -> Result<Arc<dyn Embedder>> {
    match cfg.provider {
        EmbeddingProvider::Local => Ok(Arc::new(LocalEmbedder::new(cfg))),
        EmbeddingProvider::Voyage => {
            let api_key = cfg.resolve_voyage_key()?;
            let vcfg = cfg.voyage.as_ref().cloned().unwrap_or_default();
            if !vcfg.accepted_terms {
                return Err(KbError::Config(
                    "Voyage AI provider selected but embeddings.voyage.accepted_terms \
                     is not set to true. Voyage's no-training privacy guarantee requires \
                     a payment method on file (paid tier semantics). Free tier without a \
                     card opts in to training your data. See docs/decisions/016-cloud-embedder-backend.md. \
                     Set accepted_terms = true in config.toml to acknowledge."
                        .to_string(),
                ));
            }
            let voyage = VoyageEmbedder::new(
                api_key,
                vcfg.model,
                vcfg.output_dimension,
                vcfg.request_timeout_ms,
            )?;
            Ok(Arc::new(voyage))
        }
    }
}
