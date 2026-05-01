use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use chrono::Utc;
use tracing::instrument;

use crate::db::client::SurrealClient;
use crate::error::{KbError, Result};
use crate::types::{ExportEntry, ExportEntryType, ExportResult, ImportResult};

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
            entry_type: ExportEntryType::Memory,
            data: row,
        };
        let line = serde_json::to_string(&entry)?;
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
    }

    for row in entity_rows {
        let entry = ExportEntry {
            entry_type: ExportEntryType::Entity,
            data: row,
        };
        let line = serde_json::to_string(&entry)?;
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
    }

    writer.flush()?;

    tracing::info!(
        filename = %filename,
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
/// appropriate table.  Individual bad lines are collected as errors rather than
/// aborting the whole import.
///
/// # Implementation note
///
/// SurrealDB 3.x does not support parameterised `CONTENT` in `INSERT INTO` or
/// `CREATE … CONTENT` when the bound value is a plain `serde_json::Value`
/// containing ISO-8601 datetime strings: the schema enforcer rejects strings
/// for `TYPE datetime` fields.  To work around this, we extract every field
/// from the exported JSON row and bind them as individual named parameters,
/// which allows `chrono::DateTime<Utc>` values to be sent as proper SurrealDB
/// datetime types via the SDK's serde-based binding path.
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

        match entry.entry_type {
            ExportEntryType::Memory => {
                match import_memory_row(db, entry.data).await {
                    Ok(()) => memories_imported += 1,
                    Err(e) => {
                        errors.push(format!("line {line_number}: insert memory failed: {e}"));
                    }
                }
            }
            ExportEntryType::Entity => {
                match import_entity_row(db, entry.data).await {
                    Ok(()) => entities_imported += 1,
                    Err(e) => {
                        errors.push(format!("line {line_number}: insert entity failed: {e}"));
                    }
                }
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

// ---------------------------------------------------------------------------
// Private helpers — per-table import
// ---------------------------------------------------------------------------

/// Insert a single memory row exported as a raw `serde_json::Value`.
///
/// Every field is bound as a named parameter so that `chrono::DateTime<Utc>`
/// values travel over the wire as proper SurrealDB datetime types rather than
/// raw ISO-8601 strings (which SurrealDB's schema enforcer rejects for
/// `TYPE datetime` columns).
async fn import_memory_row(db: &SurrealClient, data: serde_json::Value) -> Result<()> {
    let plain_id = str_field(&data, "id", "memory")?;

    // Datetime fields — extracted and re-bound as chrono values.
    let created_at = parse_datetime_field(&data, "created_at")?;
    let updated_at = parse_datetime_field(&data, "updated_at")?;
    let event_at   = parse_datetime_field(&data, "event_at")?;
    let expires_at     = parse_opt_datetime_field(&data, "expires_at");
    let next_review_at = parse_opt_datetime_field(&data, "next_review_at");

    // Scalar / array fields — passed through as serde_json::Value so that
    // enum strings ("semantic", "standard", etc.) are forwarded as-is.
    let summary     = str_field(&data, "summary", "memory")?;
    let content     = opt_str_field(&data, "content");
    let memory_type = str_field(&data, "memory_type", "memory")?;
    let subtype     = str_field(&data, "subtype", "memory")?;
    // Read new fields; also read legacy `source` and backfill channel/modality if missing.
    let channel     = opt_str_field(&data, "channel")
        .or_else(|| backfill_channel_from_source(&data));
    let modality    = opt_str_field(&data, "modality")
        .or_else(|| backfill_modality_from_source(&data));
    let source_ref  = opt_str_field(&data, "source_ref");
    let project     = opt_str_field(&data, "project");
    let domain      = json_field(&data, "domain").unwrap_or(serde_json::json!([]));
    let people      = json_field(&data, "people").unwrap_or(serde_json::json!([]));
    let importance  = json_field(&data, "importance").unwrap_or(serde_json::json!(3));
    let visibility  = str_field(&data, "visibility", "memory")?;
    let status      = str_field(&data, "status", "memory")?;
    let tags        = json_field(&data, "tags").unwrap_or(serde_json::json!([]));
    let access_count = json_field(&data, "access_count").unwrap_or(serde_json::json!(0));
    let source_agent = opt_str_field(&data, "source_agent");
    let session_id   = opt_str_field(&data, "session_id");

    db.query(
        "CREATE type::record('memory', $id) SET
            summary      = $summary,
            content      = $content,
            memory_type  = $memory_type,
            subtype      = $subtype,
            channel      = $channel,
            modality     = $modality,
            source_ref   = $source_ref,
            project      = $project,
            domain       = $domain,
            people       = $people,
            importance   = $importance,
            visibility   = $visibility,
            status       = $status,
            tags         = $tags,
            access_count = $access_count,
            event_at     = $event_at,
            created_at   = $created_at,
            updated_at   = $updated_at,
            expires_at   = $expires_at,
            next_review_at = $next_review_at,
            source_agent = $source_agent,
            session_id   = $session_id",
    )
    .bind(("id",           plain_id))
    .bind(("summary",      summary))
    .bind(("content",      content))
    .bind(("memory_type",  memory_type))
    .bind(("subtype",      subtype))
    .bind(("channel",      channel))
    .bind(("modality",     modality))
    .bind(("source_ref",   source_ref))
    .bind(("project",      project))
    .bind(("domain",       domain))
    .bind(("people",       people))
    .bind(("importance",   importance))
    .bind(("visibility",   visibility))
    .bind(("status",       status))
    .bind(("tags",         tags))
    .bind(("access_count", access_count))
    .bind(("event_at",     event_at))
    .bind(("created_at",   created_at))
    .bind(("updated_at",   updated_at))
    .bind(("expires_at",   expires_at))
    .bind(("next_review_at", next_review_at))
    .bind(("source_agent", source_agent))
    .bind(("session_id",   session_id))
    .await
    .map_err(|e| KbError::db("import: creating memory", e))?
    .check()
    .map_err(|e| KbError::db("import: memory schema validation", e))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Legacy source → channel/modality backfill helpers
// ---------------------------------------------------------------------------

/// Backfill `channel` from the legacy `source` field when `channel` is absent.
fn backfill_channel_from_source(data: &serde_json::Value) -> Option<String> {
    match data.get("source").and_then(|v| v.as_str()) {
        Some("manual") => Some("cli".to_string()),
        Some("slack") => Some("slack".to_string()),
        Some("discord") => Some("discord".to_string()),
        Some("chat") => Some("api".to_string()),
        Some("code") => Some("cli".to_string()),
        // meeting: channel unknown — don't backfill
        _ => None,
    }
}

/// Backfill `modality` from the legacy `source` field when `modality` is absent.
fn backfill_modality_from_source(data: &serde_json::Value) -> Option<String> {
    match data.get("source").and_then(|v| v.as_str()) {
        Some("manual") => Some("text".to_string()),
        Some("meeting") => Some("transcript".to_string()),
        Some("slack") => Some("text".to_string()),
        Some("discord") => Some("text".to_string()),
        Some("chat") => Some("text".to_string()),
        Some("code") => Some("code".to_string()),
        _ => None,
    }
}

/// Insert a single entity row exported as a raw `serde_json::Value`.
async fn import_entity_row(db: &SurrealClient, data: serde_json::Value) -> Result<()> {
    let plain_id = str_field(&data, "id", "entity")?;

    let created_at = parse_datetime_field(&data, "created_at")?;
    let updated_at = parse_datetime_field(&data, "updated_at")?;

    let name    = str_field(&data, "name", "entity")?;
    let kind    = str_field(&data, "kind", "entity")?;
    let roles   = json_field(&data, "roles").unwrap_or(serde_json::json!([]));
    let attrs   = json_field(&data, "attrs").unwrap_or(serde_json::json!({}));
    let context = opt_str_field(&data, "context");

    db.query(
        "CREATE type::record('entity', $id) SET
            name       = $name,
            kind       = $kind,
            roles      = $roles,
            attrs      = $attrs,
            context    = $context,
            created_at = $created_at,
            updated_at = $updated_at",
    )
    .bind(("id",         plain_id))
    .bind(("name",       name))
    .bind(("kind",       kind))
    .bind(("roles",      roles))
    .bind(("attrs",      attrs))
    .bind(("context",    context))
    .bind(("created_at", created_at))
    .bind(("updated_at", updated_at))
    .await
    .map_err(|e| KbError::db("import: creating entity", e))?
    .check()
    .map_err(|e| KbError::db("import: entity schema validation", e))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Private helpers — field extraction from raw JSON rows
// ---------------------------------------------------------------------------

/// Extract the plain record id (without the `{table}:` prefix) from the `id`
/// field of a raw SurrealDB export row.
fn str_field(data: &serde_json::Value, field: &str, _table: &str) -> Result<String> {
    let raw = data
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            KbError::InvalidInput(format!("export row is missing string field '{field}'"))
        })?;

    // Strip the `table:` prefix only for the `id` field.
    if field == "id" {
        // The prefix ends at the first `:`; strip everything up to and
        // including the first colon.
        let plain = raw
            .find(':')
            .map(|pos| &raw[pos + 1..])
            .unwrap_or(raw);
        Ok(plain.to_string())
    } else {
        Ok(raw.to_string())
    }
}

/// Return `Some(String)` for present non-null string fields, `None` otherwise.
fn opt_str_field(data: &serde_json::Value, field: &str) -> Option<String> {
    data.get(field)
        .and_then(|v| if v.is_null() { None } else { v.as_str() })
        .map(|s| s.to_string())
}

/// Return the raw JSON value for a field (for arrays / objects / numbers).
fn json_field(data: &serde_json::Value, field: &str) -> Option<serde_json::Value> {
    data.get(field).cloned()
}

/// Parse a required ISO-8601 datetime string field as `DateTime<Utc>`.
fn parse_datetime_field(
    data: &serde_json::Value,
    field: &str,
) -> Result<chrono::DateTime<chrono::Utc>> {
    let s = data
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            KbError::InvalidInput(format!("export row is missing datetime field '{field}'"))
        })?;
    s.parse::<chrono::DateTime<chrono::Utc>>()
        .map_err(|e| KbError::InvalidInput(format!("cannot parse '{field}' as datetime: {e}")))
}

/// Parse an optional ISO-8601 datetime string field.
/// Returns `None` when the field is absent, null, or unparseable.
fn parse_opt_datetime_field(
    data: &serde_json::Value,
    field: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    data.get(field)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok())
}
