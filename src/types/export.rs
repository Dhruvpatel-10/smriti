use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::enums::ExportEntryType;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportEntry {
    pub entry_type: ExportEntryType,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExportResult {
    pub path: String,
    pub memory_count: u64,
    pub entity_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImportResult {
    pub memories_imported: u64,
    pub entities_imported: u64,
    pub errors: Vec<String>,
}
