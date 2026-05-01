use tracing::instrument;

use crate::db::client::SurrealClient;
use crate::error::{KbError, Result};

/// Write a vector to an existing memory record.
///
/// Vectors are bound as f32 directly — fastembed produces f32, and JSON numbers
/// are type-agnostic, so SurrealDB receives identical values without the
/// allocation overhead of an f64 conversion.
#[instrument(skip(db, vector))]
pub(crate) async fn store_vector(
    db: &SurrealClient,
    memory_id: &str,
    vector: Vec<f32>,
) -> Result<()> {
    db.query("UPDATE type::record('memory', $id) SET vector = $vector")
        .bind(("id", memory_id.to_string()))
        .bind(("vector", vector))
        .await
        .map_err(|e| KbError::db(format!("storing vector for {memory_id}"), e))?;
    Ok(())
}
