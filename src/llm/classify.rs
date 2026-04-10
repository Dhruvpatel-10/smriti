use std::sync::LazyLock;

use serde::Deserialize;

use crate::error::Result;
use crate::llm::client::LlmClient;
use crate::types::{MemorySubtype, MemoryType};

/// Result of LLM-based reclassification.
#[derive(Debug, Deserialize)]
pub struct ClassifyResult {
    pub memory_type: String,
    pub subtype: String,
    pub confidence: f32,
}

/// Build the system prompt dynamically from the MemoryType/MemorySubtype enums.
/// This ensures the prompt always matches the actual taxonomy — no stale subtypes.
fn build_system_prompt() -> String {
    let mut lines = Vec::new();
    lines.push("You are a memory classification system. Given a memory summary, classify it into the correct type and subtype.".to_string());
    lines.push(String::new());
    lines.push("Valid types and subtypes:".to_string());

    for ty in MemoryType::all() {
        let subtypes: Vec<String> = MemorySubtype::all_for(*ty)
            .iter()
            .map(|st| st.to_string())
            .collect();
        lines.push(format!("- {}: {}", ty, subtypes.join(", ")));
    }

    lines.push(String::new());
    lines.push(r#"Return ONLY a JSON object with exactly these fields:"#.to_string());
    lines.push(r#"{"memory_type": "...", "subtype": "...", "confidence": 0.0-1.0}"#.to_string());

    lines.join("\n")
}

static SYSTEM_PROMPT: LazyLock<String> = LazyLock::new(build_system_prompt);

/// Classify a memory summary using LLM (Gemini primary, Groq fallback).
///
/// Returns None if no LLM is available or if classification fails.
/// This is a background, best-effort operation.
pub async fn classify_memory(
    llm: &LlmClient,
    summary: &str,
) -> Result<Option<ClassifyResult>> {
    if !llm.is_available() {
        return Ok(None);
    }

    let user_prompt = format!("Classify this memory:\n\n{summary}");

    match llm.prompt(&SYSTEM_PROMPT, &user_prompt).await? {
        Some(response) => Ok(parse_classify_response(&response)),
        None => Ok(None),
    }
}

/// Parse the LLM response, extracting JSON from potential markdown fences.
fn parse_classify_response(response: &str) -> Option<ClassifyResult> {
    let mut cleaned = response.trim();
    if let Some(stripped) = cleaned.strip_prefix("```json") {
        cleaned = stripped;
    } else if let Some(stripped) = cleaned.strip_prefix("```") {
        cleaned = stripped;
    }
    if let Some(stripped) = cleaned.strip_suffix("```") {
        cleaned = stripped;
    }
    let cleaned = cleaned.trim();

    serde_json::from_str(cleaned).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_response() {
        let response = r#"{"memory_type": "semantic", "subtype": "decision", "confidence": 0.95}"#;
        let result = parse_classify_response(response).unwrap();
        assert_eq!(result.memory_type, "semantic");
        assert_eq!(result.subtype, "decision");
        assert!(result.confidence > 0.9);
    }

    #[test]
    fn parse_fenced_json_response() {
        let response = "```json\n{\"memory_type\": \"episodic\", \"subtype\": \"meeting\", \"confidence\": 0.8}\n```";
        let result = parse_classify_response(response).unwrap();
        assert_eq!(result.memory_type, "episodic");
        assert_eq!(result.subtype, "meeting");
    }

    #[test]
    fn parse_invalid_response() {
        let result = parse_classify_response("not json at all");
        assert!(result.is_none());
    }

    #[test]
    fn system_prompt_contains_all_types() {
        let prompt = build_system_prompt();
        for ty in MemoryType::all() {
            assert!(prompt.contains(&ty.to_string()), "prompt missing type: {ty}");
        }
    }

    #[test]
    fn system_prompt_contains_all_subtypes() {
        let prompt = build_system_prompt();
        for ty in MemoryType::all() {
            for st in MemorySubtype::all_for(*ty) {
                assert!(prompt.contains(&st.to_string()), "prompt missing subtype: {st}");
            }
        }
    }
}
