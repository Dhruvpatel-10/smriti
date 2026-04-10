use chrono::Duration;

use crate::error::{KbError, Result};
use crate::types::{Importance, MemorySubtype, MemoryType};

// ── Summary quality gate ───────────────────────────────────────────────────

pub fn validate_summary(summary: &str) -> Result<()> {
    let trimmed = summary.trim();

    if trimmed.len() < 10 {
        return Err(KbError::InvalidInput(
            "summary must be at least 10 characters".to_string(),
        ));
    }
    if trimmed.len() > 500 {
        return Err(KbError::InvalidInput(
            "summary must be at most 500 characters (use content field for full text)".to_string(),
        ));
    }
    if !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return Err(KbError::InvalidInput(
            "summary must contain alphanumeric content".to_string(),
        ));
    }
    Ok(())
}

// ── Content size limit ────────────────────────────────────────────────────

/// Maximum allowed size for the content field (500 KB).
const MAX_CONTENT_BYTES: usize = 500 * 1024;

/// Validate that optional content does not exceed the size limit.
pub fn validate_content(content: Option<&str>) -> Result<()> {
    if let Some(c) = content
        && c.len() > MAX_CONTENT_BYTES
    {
        return Err(KbError::InvalidInput(format!(
            "content exceeds maximum size ({} bytes, limit {})",
            c.len(),
            MAX_CONTENT_BYTES
        )));
    }
    Ok(())
}

// ── Importance floors/ceilings ─────────────────────────────────────────────

/// Apply importance floors and ceilings per subtype.
pub fn enforce_importance(subtype: MemorySubtype, requested: Importance) -> Importance {
    let val = requested.value();
    let (floor, ceiling) = importance_bounds(subtype);
    let clamped = val.max(floor).min(ceiling);
    Importance::new(clamped).unwrap_or(requested)
}

fn importance_bounds(subtype: MemorySubtype) -> (u8, u8) {
    match subtype {
        MemorySubtype::Decision => (4, 5),
        MemorySubtype::Value => (5, 5),
        MemorySubtype::LifeGoal => (5, 5),
        MemorySubtype::ActionItem => (3, 5),
        MemorySubtype::Goal => (4, 5),
        MemorySubtype::Scratch => (1, 2),
        MemorySubtype::DailyLog => (1, 2),
        _ => (1, 5),
    }
}

// ── TTL enforcement (single source of truth) ───────────────────────────────

/// Return TTL duration for working/* subtypes. This is the single source
/// of truth for TTL values — callers compute the absolute timestamp inline.
pub fn ttl_duration(
    memory_type: MemoryType,
    subtype: MemorySubtype,
) -> Option<Duration> {
    match (memory_type, subtype) {
        (MemoryType::Working, MemorySubtype::Scratch) => Some(Duration::hours(48)),
        (MemoryType::Working, MemorySubtype::Priority) => Some(Duration::days(7)),
        (MemoryType::Working, MemorySubtype::OpenQuestion) => Some(Duration::days(30)),
        (MemoryType::Working, MemorySubtype::Signal) => Some(Duration::days(14)),
        (MemoryType::Working, MemorySubtype::Hypothesis) => Some(Duration::days(7)),
        _ => None,
    }
}

// ── Contextual embedding prepend ───────────────────────────────────────────

/// Build contextual text for embedding by prepending type/project/domain metadata.
/// This improves embedding quality by giving the model context about the memory's role.
pub fn contextual_text(
    summary: &str,
    memory_type: MemoryType,
    subtype: MemorySubtype,
    project: Option<&str>,
    domains: &[String],
) -> String {
    let mut parts = Vec::with_capacity(4);
    parts.push(format!("[{}/{}]", memory_type, subtype));
    if let Some(p) = project {
        parts.push(format!("[{}]", p));
    }
    if !domains.is_empty() {
        parts.push(format!("[{}]", domains.join(",")));
    }
    parts.push(summary.to_string());
    parts.join(" ")
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // -- Summary validation --

    #[test]
    fn summary_too_short() {
        assert!(validate_summary("short").is_err());
    }

    #[test]
    fn summary_too_long() {
        let long = "a".repeat(501);
        assert!(validate_summary(&long).is_err());
    }

    #[test]
    fn summary_no_alphanumeric() {
        assert!(validate_summary("!@#$%^&*()_+!@#$").is_err());
    }

    #[test]
    fn summary_valid() {
        assert!(validate_summary("Decided to use Stripe for payments").is_ok());
    }

    #[test]
    fn summary_at_boundaries() {
        assert!(validate_summary("1234567890").is_ok()); // exactly 10
        let exactly_500 = "a".repeat(500);
        assert!(validate_summary(&exactly_500).is_ok());
    }

    #[test]
    fn summary_short_valid() {
        assert!(validate_summary("Bug in auth").is_ok()); // 11 chars — valid now
    }

    // -- Importance enforcement --

    #[test]
    fn importance_floor_decision() {
        let imp = enforce_importance(MemorySubtype::Decision, Importance::new(1).unwrap());
        assert_eq!(imp.value(), 4);
    }

    #[test]
    fn importance_ceiling_scratch() {
        let imp = enforce_importance(MemorySubtype::Scratch, Importance::new(5).unwrap());
        assert_eq!(imp.value(), 2);
    }

    #[test]
    fn importance_value_always_five() {
        let imp = enforce_importance(MemorySubtype::Value, Importance::new(1).unwrap());
        assert_eq!(imp.value(), 5);
    }

    #[test]
    fn importance_no_enforcement_passes_through() {
        let imp = enforce_importance(MemorySubtype::Fact, Importance::new(2).unwrap());
        assert_eq!(imp.value(), 2);
    }

    // -- TTL enforcement --

    #[test]
    fn ttl_scratch_48h() {
        let result = ttl_duration(MemoryType::Working, MemorySubtype::Scratch);
        assert!(result.is_some());
        assert_eq!(result.unwrap().num_hours(), 48);
    }

    #[test]
    fn ttl_signal_14d() {
        let result = ttl_duration(MemoryType::Working, MemorySubtype::Signal);
        assert!(result.is_some());
        assert_eq!(result.unwrap().num_days(), 14);
    }

    #[test]
    fn ttl_no_enforcement_for_semantic() {
        let result = ttl_duration(MemoryType::Semantic, MemorySubtype::Fact);
        assert!(result.is_none());
    }

    // -- Contextual embedding --

    #[test]
    fn contextual_text_with_project_and_domain() {
        let text = contextual_text(
            "Decided to use Stripe", MemoryType::Semantic, MemorySubtype::Decision,
            Some("atlas"), &["startup".to_string()],
        );
        assert_eq!(text, "[semantic/decision] [atlas] [startup] Decided to use Stripe");
    }

    #[test]
    fn contextual_text_minimal() {
        let text = contextual_text(
            "A simple fact", MemoryType::Semantic, MemorySubtype::Fact,
            None, &[],
        );
        assert_eq!(text, "[semantic/fact] A simple fact");
    }
}
