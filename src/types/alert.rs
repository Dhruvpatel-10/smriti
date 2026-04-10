use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlertType {
    OverdueActionItem,
    StaleContact,
    ExpiringMemory,
    Contradiction,
}

impl fmt::Display for AlertType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OverdueActionItem => write!(f, "overdue_action_item"),
            Self::StaleContact => write!(f, "stale_contact"),
            Self::ExpiringMemory => write!(f, "expiring_memory"),
            Self::Contradiction => write!(f, "contradiction"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Alert {
    pub alert_type: AlertType,
    pub message: String,
    /// ID of the memory or entity this alert relates to.
    pub reference_id: String,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_type_display() {
        assert_eq!(format!("{}", AlertType::OverdueActionItem), "overdue_action_item");
        assert_eq!(format!("{}", AlertType::StaleContact), "stale_contact");
    }

    #[test]
    fn alert_type_serde_roundtrip() {
        let all = [
            AlertType::OverdueActionItem,
            AlertType::StaleContact,
            AlertType::ExpiringMemory,
            AlertType::Contradiction,
        ];
        for v in &all {
            let json = serde_json::to_string(v).unwrap();
            let back: AlertType = serde_json::from_str(&json).unwrap();
            assert_eq!(*v, back);
        }
    }
}
