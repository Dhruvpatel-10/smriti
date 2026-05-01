pub mod factory;
pub mod fingerprint;
pub mod local;
pub mod traits;
pub mod voyage;
pub mod voyage_batch;

pub(crate) mod store;

pub use factory::build_embedder;
pub use fingerprint::EmbedderFingerprint;
pub use local::LocalEmbedder;
pub use traits::Embedder;
pub use voyage::VoyageEmbedder;

use crate::db::client::SurrealClient;

/// Write a vector to an existing memory record.
/// Public variant for use from remember.rs dedup flow.
pub async fn store_vector_pub(
    db: &SurrealClient,
    memory_id: &str,
    vector: Vec<f32>,
) -> crate::error::Result<()> {
    store::store_vector(db, memory_id, vector).await
}
