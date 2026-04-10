use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

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
}

impl fmt::Display for MemoryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Episodic => write!(f, "episodic"),
            Self::Semantic => write!(f, "semantic"),
            Self::Procedural => write!(f, "procedural"),
            Self::Prospective => write!(f, "prospective"),
            Self::Working => write!(f, "working"),
        }
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

impl fmt::Display for MemorySubtype {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meeting => write!(f, "meeting"),
            Self::Conversation => write!(f, "conversation"),
            Self::DailyLog => write!(f, "daily_log"),
            Self::Reflection => write!(f, "reflection"),
            Self::Session => write!(f, "session"),
            Self::Research => write!(f, "research"),
            Self::Decision => write!(f, "decision"),
            Self::Fact => write!(f, "fact"),
            Self::Preference => write!(f, "preference"),
            Self::Value => write!(f, "value"),
            Self::Metric => write!(f, "metric"),
            Self::Pattern => write!(f, "pattern"),
            Self::Workflow => write!(f, "workflow"),
            Self::Skill => write!(f, "skill"),
            Self::ActionItem => write!(f, "action_item"),
            Self::Goal => write!(f, "goal"),
            Self::LifeGoal => write!(f, "life_goal"),
            Self::Experiment => write!(f, "experiment"),
            Self::Priority => write!(f, "priority"),
            Self::OpenQuestion => write!(f, "open_question"),
            Self::Scratch => write!(f, "scratch"),
            Self::Signal => write!(f, "signal"),
            Self::Hypothesis => write!(f, "hypothesis"),
        }
    }
}

impl MemorySubtype {
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
}

// ---------------------------------------------------------------------------
// Memory status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    #[default]
    Active,
    Archived,
    Superseded,
}

// ---------------------------------------------------------------------------
// Source (input channel)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Manual,
    Meeting,
    Slack,
    Discord,
    Code,
    Chat,
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

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Standard => write!(f, "standard"),
            Self::Private => write!(f, "private"),
            Self::Sensitive => write!(f, "sensitive"),
        }
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

impl fmt::Display for RelationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Updates => write!(f, "updates"),
            Self::Extends => write!(f, "extends"),
            Self::Derives => write!(f, "derives"),
            Self::Relates => write!(f, "relates"),
            Self::Involves => write!(f, "involves"),
            Self::Assigned => write!(f, "assigned"),
            Self::Belongs => write!(f, "belongs"),
        }
    }
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
    fn source_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&Source::Manual).unwrap(), "\"manual\"");
        assert_eq!(serde_json::to_string(&Source::Code).unwrap(), "\"code\"");
        assert_eq!(serde_json::to_string(&Source::Chat).unwrap(), "\"chat\"");
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
        assert_eq!(serde_json::to_string(&MemoryStatus::Archived).unwrap(), "\"archived\"");
        assert_eq!(serde_json::to_string(&MemoryStatus::Superseded).unwrap(), "\"superseded\"");
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
}
