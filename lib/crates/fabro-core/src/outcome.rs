use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Success,
    Fail,
    Skipped,
    PartialSuccess,
    Retry,
}

impl fmt::Display for StageStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Success => write!(f, "success"),
            Self::Fail => write!(f, "fail"),
            Self::Skipped => write!(f, "skipped"),
            Self::PartialSuccess => write!(f, "partial_success"),
            Self::Retry => write!(f, "retry"),
        }
    }
}

impl FromStr for StageStatus {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "success" => Ok(Self::Success),
            "fail" => Ok(Self::Fail),
            "skipped" => Ok(Self::Skipped),
            "partial_success" => Ok(Self::PartialSuccess),
            "retry" => Ok(Self::Retry),
            other => Err(format!("unknown stage status: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FailureDetail {
    pub message: String,
    pub category: Option<String>,
    pub signature: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub status: StageStatus,
    pub preferred_label: Option<String>,
    pub suggested_next_ids: Vec<String>,
    pub context_updates: HashMap<String, Value>,
    pub jump_to_node: Option<String>,
    pub notes: Option<String>,
    pub failure: Option<FailureDetail>,
    pub metadata: HashMap<String, Value>,
}

impl Default for Outcome {
    fn default() -> Self {
        Self {
            status: StageStatus::Success,
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            context_updates: HashMap::new(),
            jump_to_node: None,
            notes: None,
            failure: None,
            metadata: HashMap::new(),
        }
    }
}

impl Outcome {
    pub fn success() -> Self {
        Self::default()
    }

    pub fn fail(message: &str) -> Self {
        Self {
            status: StageStatus::Fail,
            failure: Some(FailureDetail {
                message: message.to_string(),
                category: None,
                signature: None,
            }),
            ..Self::default()
        }
    }

    pub fn skipped(reason: &str) -> Self {
        Self {
            status: StageStatus::Skipped,
            notes: Some(reason.to_string()),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct NodeResult {
    pub outcome: Outcome,
    pub duration: Duration,
    pub attempts: u32,
    pub max_attempts: u32,
}

impl NodeResult {
    pub fn new(outcome: Outcome, duration: Duration, attempts: u32, max_attempts: u32) -> Self {
        Self {
            outcome,
            duration,
            attempts,
            max_attempts,
        }
    }

    pub fn from_error(
        error: &crate::error::CoreError,
        duration: Duration,
        attempts: u32,
        max_attempts: u32,
    ) -> Self {
        Self {
            outcome: error.to_fail_outcome(),
            duration,
            attempts,
            max_attempts,
        }
    }

    pub fn from_skip(outcome: Outcome) -> Self {
        Self {
            outcome,
            duration: Duration::ZERO,
            attempts: 0,
            max_attempts: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_status_display_roundtrip() {
        let statuses = [
            StageStatus::Success,
            StageStatus::Fail,
            StageStatus::Skipped,
            StageStatus::PartialSuccess,
            StageStatus::Retry,
        ];
        for status in &statuses {
            let s = status.to_string();
            let parsed: StageStatus = s.parse().unwrap();
            assert_eq!(&parsed, status);
        }
    }

    #[test]
    fn stage_status_serde_roundtrip() {
        let statuses = [
            StageStatus::Success,
            StageStatus::Fail,
            StageStatus::Skipped,
            StageStatus::PartialSuccess,
            StageStatus::Retry,
        ];
        for status in &statuses {
            let json = serde_json::to_string(status).unwrap();
            let parsed: StageStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(&parsed, status);
        }
    }

    #[test]
    fn outcome_success_factory() {
        let o = Outcome::success();
        assert_eq!(o.status, StageStatus::Success);
        assert!(o.failure.is_none());
        assert!(o.notes.is_none());
    }

    #[test]
    fn outcome_fail_factory() {
        let o = Outcome::fail("broken");
        assert_eq!(o.status, StageStatus::Fail);
        let f = o.failure.unwrap();
        assert_eq!(f.message, "broken");
        assert!(f.category.is_none());
        assert!(f.signature.is_none());
    }

    #[test]
    fn outcome_skipped_factory() {
        let o = Outcome::skipped("not needed");
        assert_eq!(o.status, StageStatus::Skipped);
        assert_eq!(o.notes.as_deref(), Some("not needed"));
    }

    #[test]
    fn outcome_with_context_updates() {
        let mut o = Outcome::success();
        o.context_updates
            .insert("key".into(), serde_json::json!("value"));
        assert_eq!(o.context_updates["key"], serde_json::json!("value"));
    }

    #[test]
    fn outcome_with_jump() {
        let mut o = Outcome::success();
        o.jump_to_node = Some("target".into());
        assert_eq!(o.jump_to_node.as_deref(), Some("target"));
    }

    #[test]
    fn outcome_serde_roundtrip() {
        // Test that metadata (the serde-friendly field) roundtrips
        let mut o = Outcome::success();
        o.metadata
            .insert("usage".into(), serde_json::json!({"tokens": 100}));
        let json = serde_json::to_value(&o.metadata).unwrap();
        let parsed: HashMap<String, Value> = serde_json::from_value(json).unwrap();
        assert_eq!(parsed["usage"]["tokens"], 100);
    }

    #[test]
    fn failure_detail_construction() {
        let f = FailureDetail {
            message: "timeout".into(),
            category: Some("transient".into()),
            signature: Some("sig".into()),
        };
        assert_eq!(f.message, "timeout");
        assert_eq!(f.category.as_deref(), Some("transient"));
        assert_eq!(f.signature.as_deref(), Some("sig"));
    }

    #[test]
    fn node_result_from_outcome() {
        let o = Outcome::success();
        let r = NodeResult::new(o, Duration::from_millis(100), 1, 3);
        assert_eq!(r.outcome.status, StageStatus::Success);
        assert_eq!(r.duration, Duration::from_millis(100));
        assert_eq!(r.attempts, 1);
        assert_eq!(r.max_attempts, 3);
    }
}
