use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Memory type + subtype
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    Episodic,
    Semantic,
    Procedural,
    Prospective,
    Working,
}

impl MemoryType {
    /// All memory type variants, in canonical order.
    pub const fn all() -> &'static [MemoryType] {
        &[
            Self::Episodic,
            Self::Semantic,
            Self::Procedural,
            Self::Prospective,
            Self::Working,
        ]
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Procedural => "procedural",
            Self::Prospective => "prospective",
            Self::Working => "working",
        }
    }
}

impl fmt::Display for MemoryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemorySubtype {
    // Episodic (6)
    Meeting,
    Conversation,
    DailyLog,
    Reflection,
    Session,
    Research,
    // Semantic (6)
    Decision,
    Fact,
    Preference,
    Value,
    Metric,
    Pattern,
    // Procedural (2)
    Workflow,
    Skill,
    // Prospective (4)
    ActionItem,
    Goal,
    LifeGoal,
    Experiment,
    // Working (5)
    Priority,
    OpenQuestion,
    Scratch,
    Signal,
    Hypothesis,
}

impl MemorySubtype {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Meeting => "meeting",
            Self::Conversation => "conversation",
            Self::DailyLog => "daily_log",
            Self::Reflection => "reflection",
            Self::Session => "session",
            Self::Research => "research",
            Self::Decision => "decision",
            Self::Fact => "fact",
            Self::Preference => "preference",
            Self::Value => "value",
            Self::Metric => "metric",
            Self::Pattern => "pattern",
            Self::Workflow => "workflow",
            Self::Skill => "skill",
            Self::ActionItem => "action_item",
            Self::Goal => "goal",
            Self::LifeGoal => "life_goal",
            Self::Experiment => "experiment",
            Self::Priority => "priority",
            Self::OpenQuestion => "open_question",
            Self::Scratch => "scratch",
            Self::Signal => "signal",
            Self::Hypothesis => "hypothesis",
        }
    }

    /// All valid subtypes for a given type. Single source of truth —
    /// `valid_for()` and the LLM classify prompt both derive from this.
    pub const fn all_for(ty: MemoryType) -> &'static [MemorySubtype] {
        match ty {
            MemoryType::Episodic => &[
                Self::Meeting, Self::Conversation, Self::DailyLog,
                Self::Reflection, Self::Session, Self::Research,
            ],
            MemoryType::Semantic => &[
                Self::Decision, Self::Fact, Self::Preference,
                Self::Value, Self::Metric, Self::Pattern,
            ],
            MemoryType::Procedural => &[Self::Workflow, Self::Skill],
            MemoryType::Prospective => &[
                Self::ActionItem, Self::Goal, Self::LifeGoal, Self::Experiment,
            ],
            MemoryType::Working => &[
                Self::Priority, Self::OpenQuestion, Self::Scratch,
                Self::Signal, Self::Hypothesis,
            ],
        }
    }

    /// Validate that a subtype belongs to the given type.
    pub fn valid_for(self, ty: MemoryType) -> bool {
        Self::all_for(ty).contains(&self)
    }

    /// The type this subtype belongs to (every subtype has exactly one).
    pub fn memory_type(self) -> MemoryType {
        [
            MemoryType::Episodic,
            MemoryType::Semantic,
            MemoryType::Procedural,
            MemoryType::Prospective,
            MemoryType::Working,
        ]
        .into_iter()
        .find(|t| self.valid_for(*t))
        .unwrap_or(MemoryType::Semantic)
    }

    /// Subtype used when a caller gives only the type.
    pub fn default_for(ty: MemoryType) -> Self {
        match ty {
            MemoryType::Episodic => Self::Conversation,
            MemoryType::Semantic => Self::Fact,
            MemoryType::Procedural => Self::Workflow,
            MemoryType::Prospective => Self::ActionItem,
            MemoryType::Working => Self::Scratch,
        }
    }

    /// Returns true for subtypes that are immune to SM-2 decay regardless of
    /// importance. Kept here so rename refactors catch every reference.
    pub fn is_immune(self) -> bool {
        matches!(self, Self::Decision | Self::Value | Self::LifeGoal)
    }

    /// Canonical string representations of all immune subtypes, derived from
    /// the enum variants. Use as a bind parameter — never interpolate.
    pub fn immune_strs() -> Vec<&'static str> {
        [Self::Decision, Self::Value, Self::LifeGoal]
            .iter()
            .map(|s| s.as_str())
            .collect()
    }
}

impl fmt::Display for MemorySubtype {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Memory status
// ---------------------------------------------------------------------------

/// Lifecycle state of a memory record.
///
/// ADR-011 collapsed the former `Superseded` status into `Archived` — the
/// reason a memory was archived is carried by `invalidation_reason` (a
/// separate enum), not by the status. Status is a derived index hint:
/// `status='archived'` iff `invalidated_at IS NOT NONE`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    #[default]
    Active,
    Archived,
}

// ---------------------------------------------------------------------------
// Channel (where bytes came from)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Cli,
    Slack,
    Discord,
    Email,
    Sms,
    Web,
    Api,
    Github,
    VoiceApp,
    Notion,
    Calendar,
}

impl Channel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Slack => "slack",
            Self::Discord => "discord",
            Self::Email => "email",
            Self::Sms => "sms",
            Self::Web => "web",
            Self::Api => "api",
            Self::Github => "github",
            Self::VoiceApp => "voice_app",
            Self::Notion => "notion",
            Self::Calendar => "calendar",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Modality (what shape the bytes were in)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    Text,
    Transcript,
    Document,
    Code,
    Structured,
    Image,
}

impl Modality {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Transcript => "transcript",
            Self::Document => "document",
            Self::Code => "code",
            Self::Structured => "structured",
            Self::Image => "image",
        }
    }
}

impl fmt::Display for Modality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Visibility (privacy tiers)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    #[default]
    Standard,
    Private,
    Sensitive,
}

impl Visibility {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Private => "private",
            Self::Sensitive => "sensitive",
        }
    }
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Recall scope (cross-project control)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecallScope {
    /// Strict -- only this project.
    Project,
    /// This project + explicitly related projects (default).
    #[default]
    Related,
    /// Everything across all projects and domains.
    Global,
}

// ---------------------------------------------------------------------------
// Entity kind
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Person,
    Company,
    Organization,
    Project,
    Place,
    Tool,
    Concept,
    Product,
}

impl EntityKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Company => "company",
            Self::Organization => "organization",
            Self::Project => "project",
            Self::Place => "place",
            Self::Tool => "tool",
            Self::Concept => "concept",
            Self::Product => "product",
        }
    }
}

impl fmt::Display for EntityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for EntityKind {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "person" => Ok(Self::Person),
            "company" => Ok(Self::Company),
            "organization" => Ok(Self::Organization),
            "project" => Ok(Self::Project),
            "place" => Ok(Self::Place),
            "tool" => Ok(Self::Tool),
            "concept" => Ok(Self::Concept),
            "product" => Ok(Self::Product),
            other => Err(format!(
                "unknown entity kind '{other}'; valid values: person, company, organization, project, place, tool, concept, product"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Export entry type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportEntryType {
    Memory,
    Entity,
}

impl ExportEntryType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Entity => "entity",
        }
    }
}

impl fmt::Display for ExportEntryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Relation types for graph edges
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationType {
    Updates,
    Extends,
    Derives,
    Relates,
    Involves,
    Assigned,
    Belongs,
}

impl RelationType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Updates => "updates",
            Self::Extends => "extends",
            Self::Derives => "derives",
            Self::Relates => "relates",
            Self::Involves => "involves",
            Self::Assigned => "assigned",
            Self::Belongs => "belongs",
        }
    }
}

impl fmt::Display for RelationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Archive reason (for kb_archive tool — forget or retract)
// ---------------------------------------------------------------------------

/// The caller's intent when archiving a memory.
///
/// `Forget` means "no longer relevant" (sets `InvalidationReason::Forgotten`).
/// `Retract` means "I was explicitly wrong" (sets `InvalidationReason::Retracted`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveReason {
    Forget,
    Retract,
}

// ---------------------------------------------------------------------------
// Invalidation reason (bitemporal, ADR-011)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InvalidationReason {
    Superseded,   // replaced by newer memory via updates_id
    Retracted,    // user explicitly marked wrong
    Expired,      // TTL reached (working memory)
    Forgotten,    // user kb_forget — no longer relevant, not necessarily wrong
    Consolidated, // merged into another memory via kb_consolidate
}

impl InvalidationReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Superseded => "superseded",
            Self::Retracted => "retracted",
            Self::Expired => "expired",
            Self::Forgotten => "forgotten",
            Self::Consolidated => "consolidated",
        }
    }
}

impl fmt::Display for InvalidationReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for InvalidationReason {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "superseded" => Ok(Self::Superseded),
            "retracted" => Ok(Self::Retracted),
            "expired" => Ok(Self::Expired),
            "forgotten" => Ok(Self::Forgotten),
            "consolidated" => Ok(Self::Consolidated),
            other => Err(format!(
                "unknown invalidation reason '{other}'; valid values: superseded, retracted, expired, forgotten, consolidated"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// RelationKind — LLM-extracted typed relations between memories
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Causes,
    Enables,
    Blocks,
    Follows,
    Precedes,
    SimilarTo,
    Contradicts,
    Mentions,
    DependsOn,
    PartOf,
}

impl RelationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Causes => "causes",
            Self::Enables => "enables",
            Self::Blocks => "blocks",
            Self::Follows => "follows",
            Self::Precedes => "precedes",
            Self::SimilarTo => "similar_to",
            Self::Contradicts => "contradicts",
            Self::Mentions => "mentions",
            Self::DependsOn => "depends_on",
            Self::PartOf => "part_of",
        }
    }
}

impl fmt::Display for RelationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RelationKind {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "causes" => Ok(Self::Causes),
            "enables" => Ok(Self::Enables),
            "blocks" => Ok(Self::Blocks),
            "follows" => Ok(Self::Follows),
            "precedes" => Ok(Self::Precedes),
            "similar_to" => Ok(Self::SimilarTo),
            "contradicts" => Ok(Self::Contradicts),
            "mentions" => Ok(Self::Mentions),
            "depends_on" => Ok(Self::DependsOn),
            "part_of" => Ok(Self::PartOf),
            other => Err(format!(
                "unknown relation kind '{other}'; valid values: causes, enables, blocks, follows, precedes, similar_to, contradicts, mentions, depends_on, part_of"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// ExtractionStatus — tracks post-create sidecar extraction state per memory
// ---------------------------------------------------------------------------

/// Tracks whether the post-create LLM sidecar extraction pipeline has run for
/// a memory record. Set to `Pending` at create time; updated by the async task.
///
/// Invariant: `complete` is the only terminal-success state. All `*_failed`
/// variants are retryable via `--retry-failed-extraction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionStatus {
    /// Default at create time — sidecar task not yet attempted.
    Pending,
    /// Both anchor and typed-relation extraction succeeded.
    Complete,
    /// Anchor extraction errored; typed-relation may or may not have run.
    AnchorFailed,
    /// Typed-relation extraction errored; anchor succeeded.
    RelationFailed,
    /// Both anchor and typed-relation extraction errored.
    BothFailed,
    /// Circuit breaker open or sidecar process unavailable — no attempt made.
    SidecarUnavailable,
    /// Memory is Sensitive visibility — extraction skipped by policy (not a failure).
    Skipped,
}

impl ExtractionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Complete => "complete",
            Self::AnchorFailed => "anchor_failed",
            Self::RelationFailed => "relation_failed",
            Self::BothFailed => "both_failed",
            Self::SidecarUnavailable => "sidecar_unavailable",
            Self::Skipped => "skipped",
        }
    }

    /// Returns true when the status represents a failure that can be retried.
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::AnchorFailed | Self::RelationFailed | Self::BothFailed | Self::SidecarUnavailable
        )
    }
}

impl fmt::Display for ExtractionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ExtractionStatus {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "complete" => Ok(Self::Complete),
            "anchor_failed" => Ok(Self::AnchorFailed),
            "relation_failed" => Ok(Self::RelationFailed),
            "both_failed" => Ok(Self::BothFailed),
            "sidecar_unavailable" => Ok(Self::SidecarUnavailable),
            "skipped" => Ok(Self::Skipped),
            other => Err(format!(
                "unknown extraction status '{other}'; valid: pending, complete, anchor_failed, relation_failed, both_failed, sidecar_unavailable, skipped"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Reranker policy (ADR-022)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RerankerPolicy {
    Always,
    #[default]
    Adaptive,
    Never,
}

// ---------------------------------------------------------------------------
// Importance newtype (1-5)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct Importance(u8);

impl Importance {
    pub fn new(val: u8) -> Result<Self, &'static str> {
        if (1..=5).contains(&val) {
            Ok(Self(val))
        } else {
            Err("importance must be between 1 and 5")
        }
    }

    pub fn value(self) -> u8 {
        self.0
    }
}

impl Default for Importance {
    fn default() -> Self {
        Self(3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Importance -------------------------------------------------------

    // -- InvalidationReason -----------------------------------------------

    #[test]
    fn invalidation_reason_serde_roundtrip_all_variants() {
        let all = [
            InvalidationReason::Superseded,
            InvalidationReason::Retracted,
            InvalidationReason::Expired,
            InvalidationReason::Forgotten,
            InvalidationReason::Consolidated,
        ];
        for variant in &all {
            let json = serde_json::to_string(variant).unwrap();
            let back: InvalidationReason = serde_json::from_str(&json).unwrap();
            assert_eq!(*variant, back, "serde roundtrip failed for {variant:?}");
        }
    }

    #[test]
    fn invalidation_reason_from_str_roundtrip() {
        let all = [
            (InvalidationReason::Superseded, "superseded"),
            (InvalidationReason::Retracted, "retracted"),
            (InvalidationReason::Expired, "expired"),
            (InvalidationReason::Forgotten, "forgotten"),
            (InvalidationReason::Consolidated, "consolidated"),
        ];
        for (variant, s) in &all {
            let parsed: InvalidationReason = s.parse().unwrap();
            assert_eq!(parsed, *variant, "FromStr failed for '{s}'");
        }
    }

    #[test]
    fn invalidation_reason_display() {
        assert_eq!(format!("{}", InvalidationReason::Superseded), "superseded");
        assert_eq!(format!("{}", InvalidationReason::Retracted), "retracted");
        assert_eq!(format!("{}", InvalidationReason::Expired), "expired");
        assert_eq!(format!("{}", InvalidationReason::Forgotten), "forgotten");
        assert_eq!(format!("{}", InvalidationReason::Consolidated), "consolidated");
    }

    // -- Importance -------------------------------------------------------

    #[test]
    fn importance_valid_range() {
        for v in 1..=5 {
            assert!(Importance::new(v).is_ok(), "should accept {v}");
        }
    }

    #[test]
    fn importance_rejects_zero() {
        assert!(Importance::new(0).is_err());
    }

    #[test]
    fn importance_rejects_six() {
        assert!(Importance::new(6).is_err());
    }

    #[test]
    fn importance_rejects_max() {
        assert!(Importance::new(u8::MAX).is_err());
    }

    #[test]
    fn importance_default_is_three() {
        assert_eq!(Importance::default().value(), 3);
        assert_eq!(Importance::default().value(), 3);
    }

    #[test]
    fn importance_ordering() {
        let low = Importance::new(1).unwrap();
        let high = Importance::new(5).unwrap();
        assert!(low < high);
    }

    #[test]
    fn importance_serde_roundtrip() {
        let imp = Importance::new(4).unwrap();
        let json = serde_json::to_string(&imp).unwrap();
        assert_eq!(json, "4");
        let deserialized: Importance = serde_json::from_str(&json).unwrap();
        assert_eq!(imp, deserialized);
    }

    // -- MemoryType / Subtype pairing -------------------------------------

    #[test]
    fn subtype_valid_for_episodic() {
        assert!(MemorySubtype::Meeting.valid_for(MemoryType::Episodic));
        assert!(MemorySubtype::Conversation.valid_for(MemoryType::Episodic));
        assert!(MemorySubtype::DailyLog.valid_for(MemoryType::Episodic));
        assert!(MemorySubtype::Reflection.valid_for(MemoryType::Episodic));
        assert!(MemorySubtype::Session.valid_for(MemoryType::Episodic));
        assert!(MemorySubtype::Research.valid_for(MemoryType::Episodic));
    }

    #[test]
    fn subtype_valid_for_semantic() {
        assert!(MemorySubtype::Decision.valid_for(MemoryType::Semantic));
        assert!(MemorySubtype::Fact.valid_for(MemoryType::Semantic));
        assert!(MemorySubtype::Preference.valid_for(MemoryType::Semantic));
        assert!(MemorySubtype::Value.valid_for(MemoryType::Semantic));
        assert!(MemorySubtype::Metric.valid_for(MemoryType::Semantic));
        assert!(MemorySubtype::Pattern.valid_for(MemoryType::Semantic));
    }

    #[test]
    fn subtype_valid_for_procedural() {
        assert!(MemorySubtype::Workflow.valid_for(MemoryType::Procedural));
        assert!(MemorySubtype::Skill.valid_for(MemoryType::Procedural));
    }

    #[test]
    fn subtype_valid_for_prospective() {
        assert!(MemorySubtype::ActionItem.valid_for(MemoryType::Prospective));
        assert!(MemorySubtype::Goal.valid_for(MemoryType::Prospective));
        assert!(MemorySubtype::LifeGoal.valid_for(MemoryType::Prospective));
        assert!(MemorySubtype::Experiment.valid_for(MemoryType::Prospective));
    }

    #[test]
    fn subtype_valid_for_working() {
        assert!(MemorySubtype::Priority.valid_for(MemoryType::Working));
        assert!(MemorySubtype::OpenQuestion.valid_for(MemoryType::Working));
        assert!(MemorySubtype::Scratch.valid_for(MemoryType::Working));
        assert!(MemorySubtype::Signal.valid_for(MemoryType::Working));
        assert!(MemorySubtype::Hypothesis.valid_for(MemoryType::Working));
    }

    #[test]
    fn subtype_cross_type_invalid() {
        let all_pairs = [
            (MemoryType::Episodic, vec![MemorySubtype::Decision, MemorySubtype::Workflow, MemorySubtype::ActionItem, MemorySubtype::Priority]),
            (MemoryType::Semantic, vec![MemorySubtype::Meeting, MemorySubtype::Workflow, MemorySubtype::Goal, MemorySubtype::Scratch]),
            (MemoryType::Procedural, vec![MemorySubtype::Meeting, MemorySubtype::Decision, MemorySubtype::ActionItem, MemorySubtype::Hypothesis]),
            (MemoryType::Prospective, vec![MemorySubtype::Meeting, MemorySubtype::Decision, MemorySubtype::Workflow, MemorySubtype::Signal]),
            (MemoryType::Working, vec![MemorySubtype::Meeting, MemorySubtype::Decision, MemorySubtype::Workflow, MemorySubtype::ActionItem]),
        ];

        for (ty, subtypes) in &all_pairs {
            for sub in subtypes {
                assert!(
                    !sub.valid_for(*ty),
                    "{sub:?} should NOT be valid for {ty:?}"
                );
            }
        }
    }

    // -- Enum serde -------------------------------------------------------

    #[test]
    fn memory_type_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&MemoryType::Episodic).unwrap(), "\"episodic\"");
        assert_eq!(serde_json::to_string(&MemoryType::Working).unwrap(), "\"working\"");
    }

    #[test]
    fn memory_type_deserializes_snake_case() {
        let t: MemoryType = serde_json::from_str("\"semantic\"").unwrap();
        assert_eq!(t, MemoryType::Semantic);
    }

    #[test]
    fn memory_type_rejects_invalid() {
        let result = serde_json::from_str::<MemoryType>("\"invalid\"");
        assert!(result.is_err());
    }

    #[test]
    fn memory_subtype_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&MemorySubtype::DailyLog).unwrap(), "\"daily_log\"");
        assert_eq!(serde_json::to_string(&MemorySubtype::ActionItem).unwrap(), "\"action_item\"");
        assert_eq!(serde_json::to_string(&MemorySubtype::OpenQuestion).unwrap(), "\"open_question\"");
        assert_eq!(serde_json::to_string(&MemorySubtype::LifeGoal).unwrap(), "\"life_goal\"");
    }

    #[test]
    fn memory_subtype_roundtrip_all_variants() {
        let all = [
            MemorySubtype::Meeting, MemorySubtype::Conversation, MemorySubtype::DailyLog,
            MemorySubtype::Reflection, MemorySubtype::Session, MemorySubtype::Research,
            MemorySubtype::Decision, MemorySubtype::Fact,
            MemorySubtype::Preference, MemorySubtype::Value,
            MemorySubtype::Metric, MemorySubtype::Pattern,
            MemorySubtype::Workflow, MemorySubtype::Skill,
            MemorySubtype::ActionItem, MemorySubtype::Goal,
            MemorySubtype::LifeGoal, MemorySubtype::Experiment,
            MemorySubtype::Priority, MemorySubtype::OpenQuestion, MemorySubtype::Scratch,
            MemorySubtype::Signal, MemorySubtype::Hypothesis,
        ];
        for variant in &all {
            let json = serde_json::to_string(variant).unwrap();
            let back: MemorySubtype = serde_json::from_str(&json).unwrap();
            assert_eq!(*variant, back, "roundtrip failed for {variant:?}");
        }
    }

    #[test]
    fn channel_snake_case_roundtrip() {
        let all = [
            Channel::Cli, Channel::Slack, Channel::Discord, Channel::Email,
            Channel::Sms, Channel::Web, Channel::Api, Channel::Github,
            Channel::VoiceApp, Channel::Notion, Channel::Calendar,
        ];
        for variant in &all {
            let json = serde_json::to_string(variant).unwrap();
            let back: Channel = serde_json::from_str(&json).unwrap();
            assert_eq!(*variant, back, "serde roundtrip failed for {variant:?}");
            assert_eq!(variant.as_str(), variant.to_string().as_str());
        }
    }

    #[test]
    fn modality_snake_case_roundtrip() {
        let all = [
            Modality::Text, Modality::Transcript, Modality::Document,
            Modality::Code, Modality::Structured, Modality::Image,
        ];
        for variant in &all {
            let json = serde_json::to_string(variant).unwrap();
            let back: Modality = serde_json::from_str(&json).unwrap();
            assert_eq!(*variant, back, "serde roundtrip failed for {variant:?}");
            assert_eq!(variant.as_str(), variant.to_string().as_str());
        }
    }

    #[test]
    fn visibility_default_is_standard() {
        assert_eq!(Visibility::default(), Visibility::Standard);
    }

    #[test]
    fn visibility_serde_roundtrip() {
        let all = [Visibility::Standard, Visibility::Private, Visibility::Sensitive];
        for v in &all {
            let json = serde_json::to_string(v).unwrap();
            let back: Visibility = serde_json::from_str(&json).unwrap();
            assert_eq!(*v, back);
        }
    }

    #[test]
    fn recall_scope_default_is_related() {
        assert_eq!(RecallScope::default(), RecallScope::Related);
    }

    #[test]
    fn relation_type_roundtrip_all_variants() {
        let all = [
            RelationType::Updates, RelationType::Extends, RelationType::Derives,
            RelationType::Relates, RelationType::Involves, RelationType::Assigned,
            RelationType::Belongs,
        ];
        for variant in &all {
            let json = serde_json::to_string(variant).unwrap();
            let back: RelationType = serde_json::from_str(&json).unwrap();
            assert_eq!(*variant, back);
        }
    }

    #[test]
    fn relation_type_display() {
        assert_eq!(format!("{}", RelationType::Updates), "updates");
        assert_eq!(format!("{}", RelationType::Derives), "derives");
    }

    #[test]
    fn memory_status_default_is_active() {
        assert_eq!(MemoryStatus::default(), MemoryStatus::Active);
    }

    #[test]
    fn memory_status_serde() {
        assert_eq!(serde_json::to_string(&MemoryStatus::Active).unwrap(), "\"active\"");
        assert_eq!(serde_json::to_string(&MemoryStatus::Archived).unwrap(), "\"archived\"");
    }

    #[test]
    fn memory_type_display() {
        assert_eq!(format!("{}", MemoryType::Episodic), "episodic");
        assert_eq!(format!("{}", MemoryType::Prospective), "prospective");
    }

    #[test]
    fn memory_subtype_display() {
        assert_eq!(format!("{}", MemorySubtype::DailyLog), "daily_log");
        assert_eq!(format!("{}", MemorySubtype::ActionItem), "action_item");
    }

    #[test]
    fn as_str_matches_display_memory_type() {
        for &v in MemoryType::all() {
            assert_eq!(v.as_str(), v.to_string().as_str());
        }
    }

    #[test]
    fn as_str_matches_display_memory_subtype() {
        let all = [
            MemorySubtype::Meeting, MemorySubtype::Conversation, MemorySubtype::DailyLog,
            MemorySubtype::Reflection, MemorySubtype::Session, MemorySubtype::Research,
            MemorySubtype::Decision, MemorySubtype::Fact, MemorySubtype::Preference,
            MemorySubtype::Value, MemorySubtype::Metric, MemorySubtype::Pattern,
            MemorySubtype::Workflow, MemorySubtype::Skill, MemorySubtype::ActionItem,
            MemorySubtype::Goal, MemorySubtype::LifeGoal, MemorySubtype::Experiment,
            MemorySubtype::Priority, MemorySubtype::OpenQuestion, MemorySubtype::Scratch,
            MemorySubtype::Signal, MemorySubtype::Hypothesis,
        ];
        for v in all {
            assert_eq!(v.as_str(), v.to_string().as_str());
        }
    }

    #[test]
    fn as_str_matches_display_relation_type() {
        let all = [
            RelationType::Updates, RelationType::Extends, RelationType::Derives,
            RelationType::Relates, RelationType::Involves, RelationType::Assigned,
            RelationType::Belongs,
        ];
        for v in all {
            assert_eq!(v.as_str(), v.to_string().as_str());
        }
    }

    #[test]
    fn as_str_matches_display_visibility() {
        let all = [Visibility::Standard, Visibility::Private, Visibility::Sensitive];
        for v in all {
            assert_eq!(v.as_str(), v.to_string().as_str());
        }
    }

    #[test]
    fn immune_strs_returns_static_strs() {
        let strs = MemorySubtype::immune_strs();
        assert_eq!(strs, vec!["decision", "value", "life_goal"]);
    }
}
