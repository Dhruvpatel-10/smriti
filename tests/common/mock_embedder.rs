//! Deterministic content-aware embedder for tests.
//!
//! Produces vectors where similar text yields similar vectors via bag-of-character-trigrams.
//!
//! Naive random-per-string mocks fake-green dedup tests because cosine clusters around 0
//! for any two distinct random vectors. This mock keeps dedup thresholds
//! (0.82 warn / 0.92 reject) exercising real semantic similarity behaviour.

use async_trait::async_trait;
use smriti::db::embeddings::fingerprint::EmbedderFingerprint;
use smriti::db::embeddings::Embedder;
use smriti::error::Result;

/// Deterministic, content-aware embedder for tests.
///
/// Vector construction: bag-of-character-trigrams hashed into `dim` buckets,
/// then L2-normalised. Identical text → identical vectors → cosine 1.0.
/// Similar text → overlapping trigrams → cosine > 0.
pub struct MockEmbedder {
    dim: usize,
    fingerprint: EmbedderFingerprint,
    /// When `true`, `max_chunks_per_doc()` returns 1000 (triggers sibling-batch path).
    contextualized: bool,
}

impl MockEmbedder {
    /// Build a MockEmbedder with the given fingerprint string in `provider/model/dim` form.
    ///
    /// # Panics
    ///
    /// Panics if `fingerprint_str` is not parseable.
    pub fn new(fingerprint_str: &str) -> Self {
        let parts: Vec<&str> = fingerprint_str.splitn(3, '/').collect();
        assert_eq!(
            parts.len(),
            3,
            "fingerprint must be provider/model/dim, got: {fingerprint_str}"
        );
        let dim: usize = parts[2].parse().expect("dim must be a number");
        Self {
            dim,
            fingerprint: EmbedderFingerprint {
                provider: parts[0].to_string(),
                model: parts[1].to_string(),
                dimensions: dim,
            },
            contextualized: false,
        }
    }

    /// Build with default `mock/v1/1024` fingerprint.
    pub fn default_1024() -> Self {
        Self::new("mock/v1/1024")
    }

    /// Builder: when `enabled`, `max_chunks_per_doc()` returns 1000 so the
    /// sibling-batch path in `reembed_all` / `chunk_and_embed` is exercised.
    pub fn with_contextualized_support(mut self, enabled: bool) -> Self {
        self.contextualized = enabled;
        self
    }

    /// Compute a trigram bag-of-words vector for `text` and L2-normalise it.
    ///
    /// Each character-trigram is mapped to a bucket in `0..dim` via a simple
    /// polynomial hash. The resulting raw count vector is then divided by its
    /// L2 norm so that cosine similarity equals the dot product.
    fn compute_vector(&self, text: &str) -> Vec<f32> {
        let mut counts = vec![0u64; self.dim];

        let chars: Vec<char> = text.chars().collect();
        if chars.len() < 3 {
            // For very short text, fall back to single characters + bigrams.
            for (i, &c) in chars.iter().enumerate() {
                let bucket = (c as usize * 31 + i) % self.dim;
                counts[bucket] += 1;
            }
        } else {
            for window in chars.windows(3) {
                let h = trigram_hash(window[0], window[1], window[2]);
                counts[h % self.dim] += 1;
            }
        }

        let mut v: Vec<f32> = counts.iter().map(|&c| c as f32).collect();
        l2_normalize(&mut v);
        v
    }
}

/// Polynomial hash for a character trigram.
fn trigram_hash(a: char, b: char, c: char) -> usize {
    (a as usize)
        .wrapping_mul(31_usize.wrapping_pow(2))
        .wrapping_add((b as usize).wrapping_mul(31))
        .wrapping_add(c as usize)
}

/// L2-normalise `v` in place. No-op if all zeros (avoids div-by-zero).
fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|&x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[async_trait]
impl Embedder for MockEmbedder {
    fn fingerprint(&self) -> EmbedderFingerprint {
        self.fingerprint.clone()
    }

    fn dimensions(&self) -> usize {
        self.dim
    }

    fn max_input_tokens(&self) -> usize {
        32000
    }

    fn max_chunks_per_doc(&self) -> usize {
        if self.contextualized { 1000 } else { 1 }
    }

    fn recommended_chunk_tokens(&self) -> usize {
        350
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.compute_vector(text))
    }

    async fn embed_document(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.compute_vector(text))
    }

    async fn embed_document_chunks(&self, chunks: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(chunks.len());
        for c in chunks {
            out.push(self.compute_vector(c));
        }
        Ok(out)
    }
}

// ── Cosine similarity helper ──────────────────────────────────────────────────

/// Cosine similarity between two f32 vectors. Panics if lengths differ.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "vector dimension mismatch");
    a.iter().zip(b.iter()).map(|(&x, &y)| x * y).sum()
}

// ── Inline tests for the mock itself ─────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_identical_text_yields_identical_vectors() {
        let e = MockEmbedder::default_1024();
        let text = "the quick brown fox jumps over the lazy dog";
        let v1 = e.compute_vector(text);
        let v2 = e.compute_vector(text);
        assert_eq!(v1.len(), 1024);
        assert_eq!(v1, v2, "identical text must produce identical vectors");
    }

    #[test]
    fn mock_similar_text_yields_high_cosine() {
        let e = MockEmbedder::default_1024();
        // Trigram bag-of-words on short inputs gives modest overlap — pick
        // longer, more redundant fixtures so similar text clearly clusters
        // above the dissimilar baseline.
        let a = e.compute_vector(
            "the cat sat on the mat and the cat looked at the mat with curiosity",
        );
        let b = e.compute_vector(
            "a cat is on a mat and the cat watches the mat with curiosity",
        );
        let sim = cosine(&a, &b);
        assert!(
            sim > 0.5,
            "similar sentences should have cosine > 0.5; got {sim:.4}"
        );
    }

    #[test]
    fn mock_dissimilar_text_yields_low_cosine() {
        let e = MockEmbedder::default_1024();
        let a = e.compute_vector("weather is sunny today");
        let b = e.compute_vector("Rust borrow checker rules");
        let sim = cosine(&a, &b);
        assert!(
            sim < 0.3,
            "dissimilar sentences should have cosine < 0.3; got {sim:.4}"
        );
    }

    #[test]
    fn mock_vector_is_unit_length() {
        let e = MockEmbedder::default_1024();
        let v = e.compute_vector("normalisation test with some content");
        let norm: f32 = v.iter().map(|&x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-5,
            "vector must be unit-length; ||v|| = {norm:.6}"
        );
    }

    #[test]
    fn mock_contextualized_support_builder() {
        let e = MockEmbedder::default_1024().with_contextualized_support(true);
        assert_eq!(e.max_chunks_per_doc(), 1000);
    }

    #[test]
    fn mock_default_max_chunks_per_doc_is_one() {
        let e = MockEmbedder::default_1024();
        assert_eq!(e.max_chunks_per_doc(), 1);
    }
}
