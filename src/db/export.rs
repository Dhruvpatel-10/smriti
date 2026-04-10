use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use chrono::Utc;
use tracing::instrument;

use crate::db::client::SurrealClient;
use crate::error::{KbError, Result};
use crate::types::{ExportEntry, ExportResult, ImportResult};

/// Export all memories (and optionally entities) to a JSONL backup file.
///
/// Creates the export directory if it doesn't exist, then writes one JSON
/// object per line in the form `{"entry_type": "memory"|"entity", "data": {...}}`.
/// Returns a summary with the file path and record counts.
#[instrument(skip(db), fields(exclude_sensitive))]
pub async fn export_all(
    db: &SurrealClient,
    export_dir: &Path,
    exclude_sensitive: bool,
) -> Result<ExportResult> {
    std::fs::create_dir_all(export_dir)?;

    let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
    let filename = format!("kb_export_{timestamp}.jsonl");
    let file_path = export_dir.join(&filename);

    // --- Query memories ---
    let memory_sql = if exclude_sensitive {
        "SELECT * FROM memory WHERE visibility != 'sensitive'"
    } else {
        "SELECT * FROM memory"
    };

    let mut mem_result = db
        .query(memory_sql)
        .await
        .map_err(|e| KbError::db("export: querying memories", e))?;

    let memory_rows: Vec<serde_json::Value> = mem_result
        .take(0)
        .map_err(|e| KbError::db("export: deserializing memory rows", e))?;

    // --- Query entities ---
    let mut ent_result = db
        .query("SELECT * FROM entity")
        .await
        .map_err(|e| KbError::db("export: querying entities", e))?;

    let entity_rows: Vec<serde_json::Value> = ent_result
        .take(0)
        .map_err(|e| KbError::db("export: deserializing entity rows", e))?;

    let memory_count = memory_rows.len() as u64;
    let entity_count = entity_rows.len() as u64;

    // --- Write JSONL ---
    let file = std::fs::File::create(&file_path)?;
    let mut writer = BufWriter::new(file);

    for row in memory_rows {
        let entry = ExportEntry {
            entry_type: "memory".to_string(),
            data: row,
        };
        let line = serde_json::to_string(&entry)?;
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
    }

    for row in entity_rows {
        let entry = ExportEntry {
            entry_type: "entity".to_string(),
            data: row,
        };
        let line = serde_json::to_string(&entry)?;
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
    }

    writer.flush()?;

    tracing::info!(
        path = %file_path.display(),
        memory_count,
        entity_count,
        "export complete"
    );

    Ok(ExportResult {
        path: file_path.to_string_lossy().into_owned(),
        memory_count,
        entity_count,
    })
}

/// Import records from a JSONL backup file produced by [`export_all`].
///
/// Reads each line, parses it as an [`ExportEntry`], and inserts it into the
/// appropriate table using `INSERT INTO … CONTENT` to preserve original IDs and
/// timestamps.  Individual bad lines are collected as errors rather than
/// aborting the whole import.
#[instrument(skip(db), fields(path = %path.display()))]
pub async fn import_from(db: &SurrealClient, path: &Path) -> Result<ImportResult> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);

    let mut memories_imported: u64 = 0;
    let mut entities_imported: u64 = 0;
    let mut errors: Vec<String> = Vec::new();

    for (line_num, line_result) in reader.lines().enumerate() {
        let line_number = line_num + 1; // 1-indexed for error messages

        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                errors.push(format!("line {line_number}: io error reading line: {e}"));
                continue;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let entry: ExportEntry = match serde_json::from_str(trimmed) {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("line {line_number}: failed to parse entry: {e}"));
                continue;
            }
        };

        match entry.entry_type.as_str() {
            "memory" => {
                match db
                    .query("INSERT INTO memory CONTENT $data")
                    .bind(("data", entry.data))
                    .await
                {
                    Ok(_) => memories_imported += 1,
                    Err(e) => {
                        errors.push(format!("line {line_number}: insert memory failed: {e}"));
                    }
                }
            }
            "entity" => {
                match db
                    .query("INSERT INTO entity CONTENT $data")
                    .bind(("data", entry.data))
                    .await
                {
                    Ok(_) => entities_imported += 1,
                    Err(e) => {
                        errors.push(format!("line {line_number}: insert entity failed: {e}"));
                    }
                }
            }
            other => {
                errors.push(format!(
                    "line {line_number}: unknown entry_type '{other}', skipping"
                ));
            }
        }
    }

    tracing::info!(
        memories_imported,
        entities_imported,
        error_count = errors.len(),
        "import complete"
    );

    Ok(ImportResult {
        memories_imported,
        entities_imported,
        errors,
    })
}
