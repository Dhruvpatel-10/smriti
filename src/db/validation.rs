use chrono::Duration;

use crate::error::{KbError, Result};
use crate::types::{Channel, Importance, MemorySubtype, MemoryType, Modality};

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

// ── Static-memory classifier ───────────────────────────────────────────────

/// Decide whether a memory should be flagged `is_static` based on subtype +
/// importance. Static memories are permanent facts about the user (preferences,
/// values, life goals, high-importance facts) — surfaced first by `kb_profile`,
/// excluded from importance-decay pruning, and never gain a TTL.
///
/// This is a default — callers may override via the explicit `is_static` field
/// on `kb_remember`. Mirrors the migration_010 backfill rule.
pub fn classify_static(subtype: MemorySubtype, importance: Importance) -> bool {
    match subtype {
        MemorySubtype::Value | MemorySubtype::LifeGoal | MemorySubtype::Preference => true,
        MemorySubtype::Fact => importance.value() >= 4,
        _ => false,
    }
}

// ── Contextual embedding prepend ───────────────────────────────────────────

/// Build contextual text for embedding by prepending channel/modality/type/project/domain/people
/// metadata. Including `people` raises recall on entity-anchored queries — the
/// dense embedder and the BM25 index both index this string, so a question
/// like "what does Arjun think about X" can match memories whose summary does
/// not contain "Arjun" but whose `people` field does.
///
/// Format: `[channel/modality] [type/subtype] [project] [domains] [people: ...] summary`
/// The `[channel/modality]` prefix is omitted when both are `None`. When only one is
/// present, `-` is used as a placeholder so the embedder sees a stable token shape.
#[allow(clippy::too_many_arguments)]
pub fn contextual_text(
    summary: &str,
    memory_type: MemoryType,
    subtype: MemorySubtype,
    project: Option<&str>,
    domains: &[String],
    people: &[String],
    channel: Option<Channel>,
    modality: Option<Modality>,
) -> String {
    let mut parts = Vec::with_capacity(6);
    match (channel, modality) {
        (None, None) => {}
        (Some(c), Some(m)) => parts.push(format!("[{}/{}]", c.as_str(), m.as_str())),
        (Some(c), None) => parts.push(format!("[{}/{}]", c.as_str(), "-")),
        (None, Some(m)) => parts.push(format!("[{}/{}]", "-", m.as_str())),
    }
    parts.push(format!("[{}/{}]", memory_type, subtype));
    if let Some(p) = project {
        parts.push(format!("[{}]", p));
    }
    if !domains.is_empty() {
        parts.push(format!("[{}]", domains.join(",")));
    }
    if !people.is_empty() {
        parts.push(format!("[people: {}]", people.join(",")));
    }
    parts.push(summary.to_string());
    parts.join(" ")
}

/// Characters of `content` appended to the embed text (P0.8). 0 = summary only.
/// Set once at startup from `[embeddings].content_prefix_chars`; changing it
/// changes what stored vectors represent, so run `kb_reembed` afterwards.
static EMBED_CONTENT_CHARS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn set_embed_content_chars(n: usize) {
    EMBED_CONTENT_CHARS.store(n, std::sync::atomic::Ordering::Relaxed);
}

/// Append up to [`EMBED_CONTENT_CHARS`] characters of `content` to the
/// contextual text. Shared by the write path and `kb_reembed` so both embed
/// identical text.
pub fn with_content_prefix(contextual: String, content: Option<&str>) -> String {
    append_content_prefix(contextual, content, EMBED_CONTENT_CHARS.load(std::sync::atomic::Ordering::Relaxed))
}

fn append_content_prefix(mut contextual: String, content: Option<&str>, max_chars: usize) -> String {
    if max_chars == 0 {
        return contextual;
    }
    if let Some(c) = content.map(str::trim).filter(|c| !c.is_empty()) {
        contextual.push('\n');
        contextual.extend(c.chars().take(max_chars));
    }
    contextual
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #[test]
    fn content_prefix_off_returns_contextual_unchanged() {
        assert_eq!(append_content_prefix("ctx".into(), Some("body"), 0), "ctx");
    }

    #[test]
    fn content_prefix_truncates_on_char_boundary() {
        assert_eq!(append_content_prefix("ctx".into(), Some("  héllo world "), 5), "ctx\nhéllo");
        assert_eq!(append_content_prefix("ctx".into(), None, 5), "ctx");
        assert_eq!(append_content_prefix("ctx".into(), Some("   "), 5), "ctx");
    }

    use super::*;

    // -- Content validation --

    #[test]
    fn content_over_limit_rejected() {
        let big = "x".repeat(MAX_CONTENT_BYTES + 1);
        let result = validate_content(Some(&big));
        assert!(result.is_err(), "content above limit must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("content exceeds"), "error message should mention content exceeds: {msg}");
    }

    #[test]
    fn content_at_limit_accepted() {
        let exactly = "x".repeat(MAX_CONTENT_BYTES);
        assert!(validate_content(Some(&exactly)).is_ok());
    }

    #[test]
    fn content_none_accepted() {
        assert!(validate_content(None).is_ok());
    }

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

    // -- Unicode summary boundary tests --
    //
    // `validate_summary` uses byte length (trimmed.len()), not char count.
    // These tests document the current byte-length semantics.
    // "あいうえお" = 5 chars × 3 bytes = 15 bytes → passes the ≥10 byte gate.
    // "あい"       = 2 chars × 3 bytes = 6 bytes  → fails (< 10 bytes).

    #[test]
    fn validate_summary_accepts_15_byte_unicode() {
        // 5 hiragana chars × 3 bytes each = 15 bytes — should pass
        let s = "あいうえお";
        assert_eq!(s.len(), 15, "precondition: 15 bytes");
        assert!(
            validate_summary(s).is_ok(),
            "15-byte unicode summary should pass the ≥10 byte gate"
        );
    }

    #[test]
    fn validate_summary_rejects_6_byte_unicode() {
        // 2 hiragana chars × 3 bytes each = 6 bytes — should fail
        let s = "あい";
        assert_eq!(s.len(), 6, "precondition: 6 bytes");
        assert!(
            validate_summary(s).is_err(),
            "6-byte unicode summary should fail the ≥10 byte gate (byte-length semantics)"
        );
    }

    #[test]
    fn contextual_text_with_project_and_domain() {
        let text = contextual_text(
            "Decided to use Stripe", MemoryType::Semantic, MemorySubtype::Decision,
            Some("atlas"), &["startup".to_string()], &[], None, None,
        );
        assert_eq!(text, "[semantic/decision] [atlas] [startup] Decided to use Stripe");
    }

    #[test]
    fn contextual_text_minimal() {
        let text = contextual_text(
            "A simple fact", MemoryType::Semantic, MemorySubtype::Fact,
            None, &[], &[], None, None,
        );
        assert_eq!(text, "[semantic/fact] A simple fact");
    }

    #[test]
    fn contextual_text_includes_people() {
        let text = contextual_text(
            "Played chess; rating rose to 1485",
            MemoryType::Episodic, MemorySubtype::DailyLog,
            None, &[], &["Arjun".to_string(), "Priya".to_string()], None, None,
        );
        assert_eq!(
            text,
            "[episodic/daily_log] [people: Arjun,Priya] Played chess; rating rose to 1485"
        );
    }

    #[test]
    fn contextual_text_full_with_people() {
        let text = contextual_text(
            "Decided to use Stripe",
            MemoryType::Semantic, MemorySubtype::Decision,
            Some("atlas"), &["startup".to_string()], &["Bob".to_string()], None, None,
        );
        assert_eq!(
            text,
            "[semantic/decision] [atlas] [startup] [people: Bob] Decided to use Stripe"
        );
    }

    #[test]
    fn contextual_text_with_channel_and_modality() {
        use crate::types::{Channel, Modality};
        let text = contextual_text(
            "Meeting notes summary",
            MemoryType::Episodic, MemorySubtype::Meeting,
            None, &[], &[], Some(Channel::Slack), Some(Modality::Transcript),
        );
        assert_eq!(text, "[slack/transcript] [episodic/meeting] Meeting notes summary");
    }

    #[test]
    fn contextual_text_channel_only() {
        use crate::types::Channel;
        let text = contextual_text(
            "Quick note", MemoryType::Semantic, MemorySubtype::Fact,
            None, &[], &[], Some(Channel::Cli), None,
        );
        assert_eq!(text, "[cli/-] [semantic/fact] Quick note");
    }

    #[test]
    fn contextual_text_modality_only() {
        use crate::types::Modality;
        let text = contextual_text(
            "Code review findings", MemoryType::Semantic, MemorySubtype::Fact,
            None, &[], &[], None, Some(Modality::Code),
        );
        assert_eq!(text, "[-/code] [semantic/fact] Code review findings");
    }
}
