use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

use super::ids::AgentId;

// ---------------------------------------------------------------------------
// AgentKind
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Assistant,
    Reviewer,
    Summarizer,
    Researcher,
    Custom,
}

impl AgentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Assistant => "assistant",
            Self::Reviewer => "reviewer",
            Self::Summarizer => "summarizer",
            Self::Researcher => "researcher",
            Self::Custom => "custom",
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AgentKind {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "assistant" => Ok(Self::Assistant),
            "reviewer" => Ok(Self::Reviewer),
            "summarizer" => Ok(Self::Summarizer),
            "researcher" => Ok(Self::Researcher),
            "custom" => Ok(Self::Custom),
            other => Err(format!(
                "unknown agent kind '{other}'; valid values: assistant, reviewer, summarizer, researcher, custom"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// AgentStatus — only active|archived (no superseded)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    #[default]
    Active,
    Archived,
}

impl AgentStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }
}

impl fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Agent — full record as stored in DB
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub kind: AgentKind,
    pub model: Option<String>,
    pub version: Option<String>,
    pub parent_agent: Option<AgentId>,
    pub project: Option<String>,
    pub config: Option<serde_json::Value>,
    pub status: AgentStatus,
    pub spawned_at: DateTime<Utc>,
    pub retired_at: Option<DateTime<Utc>>,
    pub last_active: DateTime<Utc>,
}

/// Alias following the entity.rs `EntityFull` pattern.
pub type AgentFull = Agent;

// ---------------------------------------------------------------------------
// AgentSummary — lightweight list view
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentSummary {
    pub id: AgentId,
    pub name: String,
    pub kind: AgentKind,
    pub status: AgentStatus,
    pub project: Option<String>,
    pub last_active: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_has_correct_prefix() {
        let id = AgentId::new();
        assert!(id.as_str().starts_with("agent_"), "got: {}", id);
    }

    #[test]
    fn agent_id_roundtrip() {
        let id = AgentId::new();
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: AgentId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, deserialized);
    }

    #[test]
    fn agent_id_from_string_roundtrip() {
        let s = "agent_abc123".to_string();
        let id = AgentId::from_string(s.clone());
        assert_eq!(id.as_str(), s.as_str());
        assert_eq!(format!("{id}"), s);
    }

    #[test]
    fn agent_kind_serde_roundtrip_all_variants() {
        let all = [
            AgentKind::Assistant,
            AgentKind::Reviewer,
            AgentKind::Summarizer,
            AgentKind::Researcher,
            AgentKind::Custom,
        ];
        for variant in &all {
            let json = serde_json::to_string(variant).unwrap();
            let back: AgentKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*variant, back, "serde roundtrip failed for {variant:?}");
        }
    }

    #[test]
    fn agent_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&AgentKind::Assistant).unwrap(),
            "\"assistant\""
        );
        assert_eq!(
            serde_json::to_string(&AgentKind::Summarizer).unwrap(),
            "\"summarizer\""
        );
        assert_eq!(
            serde_json::to_string(&AgentKind::Custom).unwrap(),
            "\"custom\""
        );
    }

    #[test]
    fn agent_kind_from_str_all_variants() {
        let pairs = [
            ("assistant", AgentKind::Assistant),
            ("reviewer", AgentKind::Reviewer),
            ("summarizer", AgentKind::Summarizer),
            ("researcher", AgentKind::Researcher),
            ("custom", AgentKind::Custom),
        ];
        for (s, expected) in &pairs {
            let parsed: AgentKind = s.parse().unwrap();
            assert_eq!(parsed, *expected);
        }
    }

    #[test]
    fn agent_kind_from_str_rejects_invalid() {
        assert!("unknown_kind".parse::<AgentKind>().is_err());
        assert!("".parse::<AgentKind>().is_err());
    }
}
