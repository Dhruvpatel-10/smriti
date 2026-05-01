use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::PathBuf;

use crate::error::{KbError, Result};
use crate::types::QueueEntry;

/// Hard cap on queue file size to prevent memory exhaustion before the line count
/// check runs.  1 MiB is far above any realistic pending.jsonl.
const MAX_QUEUE_FILE_BYTES: u64 = 1_048_576;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct QueueInput {
    /// Natural language instruction for the memory session
    pub instruction: String,
    /// Which project is calling this
    pub source_project: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct QueueOutput {
    pub message: String,
}

#[tracing::instrument(
    name = "kb_queue",
    skip_all,
    fields(
        source_project = %input.source_project,
        instruction_len = input.instruction.len(),
    )
)]
pub async fn execute(queue_dir: &PathBuf, max_size: usize, input: QueueInput) -> Result<QueueOutput> {
    let path = queue_dir.join("pending.jsonl");

    // Ensure queue dir exists
    std::fs::create_dir_all(queue_dir)?;

    // Check queue size using a bounded read so a large file cannot exhaust
    // memory before the line count fires (TOCTOU-safe: we own the bytes we check).
    if path.exists() {
        let file = std::fs::File::open(&path)?;
        let mut reader = file.take(MAX_QUEUE_FILE_BYTES + 1);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;
        if buf.len() as u64 > MAX_QUEUE_FILE_BYTES {
            return Err(KbError::Queue(format!(
                "queue file exceeds {MAX_QUEUE_FILE_BYTES} bytes"
            )));
        }
        let content = String::from_utf8_lossy(&buf);
        let line_count = content.lines().filter(|l| !l.trim().is_empty()).count();
        if line_count >= max_size {
            return Err(KbError::Queue(format!(
                "queue full ({line_count}/{max_size} entries)"
            )));
        }
    }

    let entry = QueueEntry {
        instruction: input.instruction,
        source_project: input.source_project,
        queued_at: Utc::now(),
    };

    let json = serde_json::to_string(&entry)?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;

    writeln!(file, "{json}")?;

    let instruction_len = entry.instruction.len();
    let src = &entry.source_project;
    tracing::info!(
        source_project = %src,
        instruction_len,
        "queued instruction"
    );

    Ok(QueueOutput {
        message: "Instruction queued for memory session".to_string(),
    })
}
