use std::path::Path;

use surrealdb::Surreal;
use surrealdb::engine::local::{Db, SurrealKv};
use tracing::{info, instrument};

use crate::db::migrations;
use crate::error::{KbError, Result};

pub type SurrealClient = Surreal<Db>;

/// Connect to embedded SurrealKV and run any pending schema migrations.
#[instrument(skip_all, fields(path = %path.as_ref().display()))]
pub async fn connect(path: impl AsRef<Path>) -> Result<SurrealClient> {
    let path = path.as_ref();
    std::fs::create_dir_all(path).map_err(|e| {
        KbError::Config(format!("cannot create db dir {}: {e}", path.display()))
    })?;

    let db = Surreal::new::<SurrealKv>(path)
        .await
        .map_err(|e| KbError::db("connecting to SurrealKV", e))?;

    db.use_ns("kb")
        .use_db("main")
        .await
        .map_err(|e| KbError::db("selecting namespace/database", e))?;

    migrations::run(&db).await?;
    info!("database ready (schema v{})", migrations::latest_version());
    Ok(db)
}
