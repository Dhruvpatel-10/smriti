use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{client::SurrealClient, export, path_safety};
use crate::error::Result;
use crate::types::*;

// ---------------------------------------------------------------------------
// kb_export
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExportInput {
    /// Optional filename for the export (uses auto-generated name if not provided).
    /// Must be a plain filename — no paths or directory components allowed.
    pub filename: Option<String>,
    /// Exclude sensitive-visibility memories (default true)
    pub exclude_sensitive: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExportOutput {
    pub result: ExportResult,
}

#[tracing::instrument(skip_all, fields(tool = "kb_export"))]
pub async fn execute_export(
    db: &SurrealClient,
    export_dir: &std::path::Path,
    exclude_sensitive_default: bool,
    input: ExportInput,
) -> Result<ExportOutput> {
    // Validate filename if provided; always write to the configured export_dir.
    if let Some(ref name) = input.filename {
        path_safety::safe_filename(name)?;
    }
    let exclude = input.exclude_sensitive.unwrap_or(exclude_sensitive_default);

    let result = export::export_all(db, export_dir, exclude).await?;
    Ok(ExportOutput { result })
}

// ---------------------------------------------------------------------------
// kb_import
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImportInput {
    /// Filename of the JSONL export file to import (must be in the export directory).
    /// Plain filename only — no paths or directory components allowed.
    pub filename: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ImportOutput {
    pub result: ImportResult,
}

#[tracing::instrument(skip_all, fields(tool = "kb_import"))]
pub async fn execute_import(
    db: &SurrealClient,
    export_dir: &std::path::Path,
    input: ImportInput,
) -> Result<ImportOutput> {
    // Validate filename and resolve within the configured export directory.
    let safe_name = path_safety::safe_filename(&input.filename)?;
    let path = export_dir.join(safe_name);
    let result = export::import_from(db, &path).await?;
    Ok(ImportOutput { result })
}
