use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

use crate::error::{KbError, Result};
use crate::types::QueueEntry;

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

#[tracing::instrument(skip_all, fields(tool = "kb_queue"))]
pub async fn execute(queue_dir: &PathBuf, max_size: usize, input: QueueInput) -> Result<QueueOutput> {
    let path = queue_dir.join("pending.jsonl");

    // Ensure queue dir exists
    std::fs::create_dir_all(queue_dir)?;

    // Check queue size
    if path.exists() {
        let content = std::fs::read_to_string(&path)?;
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

    Ok(QueueOutput {
        message: "Instruction queued for memory session".to_string(),
    })
}
