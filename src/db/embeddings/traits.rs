use async_trait::async_trait;

use crate::db::embeddings::fingerprint::EmbedderFingerprint;
use crate::error::Result;

#[async_trait]
pub trait Embedder: Send + Sync + 'static {
    fn fingerprint(&self) -> EmbedderFingerprint;
    fn dimensions(&self) -> usize;
    fn max_input_tokens(&self) -> usize;
    fn max_chunks_per_doc(&self) -> usize {
        1
    }
    fn recommended_chunk_tokens(&self) -> usize {
        350
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
    async fn embed_document(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed N chunks of the same parent doc.
    ///
    /// Default impl runs N independent `embed_document` calls in sequence.
    /// Backends with native contextualized-batch support override.
    ///
    /// **Contract**: when `max_chunks_per_doc() == 1`, the caller is responsible
    /// for applying any contextualization (e.g., structural prefix prepend) to
    /// each string before calling this method. The default impl embeds inputs
    /// as-given.
    ///
    /// The sequential loop is intentional — `&self` cannot be moved across a
    /// `tokio::spawn` boundary, and CPU-bound local backends serialise through
    /// a single mutex anyway. Do not "optimise" with `try_join_all`.
    async fn embed_document_chunks(&self, chunks: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(chunks.len());
        for c in chunks {
            out.push(self.embed_document(c).await?);
        }
        Ok(out)
    }
}
