use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{classify_failure_reason, FailureClass};

/// Status of a pipeline stage execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Success,
    Fail,
    PartialSuccess,
    Retry,
    Skipped,
}

impl fmt::Display for StageStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Success => "success",
            Self::Fail => "fail",
            Self::PartialSuccess => "partial_success",
            Self::Retry => "retry",
            Self::Skipped => "skipped",
        };
        write!(f, "{s}")
    }
}

impl FromStr for StageStatus {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "success" => Ok(Self::Success),
            "fail" => Ok(Self::Fail),
            "partial_success" => Ok(Self::PartialSuccess),
            "retry" => Ok(Self::Retry),
            "skipped" => Ok(Self::Skipped),
            other => Err(format!("unknown stage status: {other}")),
        }
    }
}

/// Token usage from a single pipeline stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageUsage {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

impl From<&StageUsage> for fabro_llm::types::Usage {
    fn from(u: &StageUsage) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            total_tokens: u.input_tokens + u.output_tokens,
            cache_read_tokens: u.cache_read_tokens,
            cache_write_tokens: u.cache_write_tokens,
            reasoning_tokens: u.reasoning_tokens,
            speed: u.speed.clone(),
            raw: None,
        }
    }
}

/// Structured failure information carried through the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureDetail {
    pub message: String,
    pub failure_class: FailureClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_signature: Option<String>,
}

impl FailureDetail {
    pub fn new(message: impl Into<String>, failure_class: FailureClass) -> Self {
        Self {
            message: message.into(),
            failure_class,
            failure_signature: None,
        }
    }
}

/// The result of executing a node handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub status: StageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_next_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub context_updates: HashMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<StageUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_touched: Vec<String>,
    /// When set, the engine bypasses edge selection and jumps directly to this node.
    /// Used by the parallel handler to skip re-executing branch nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump_to_node: Option<String>,
    /// Wall-clock duration of the stage execution in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl Outcome {
    #[must_use]
    pub fn success() -> Self {
        Self {
            status: StageStatus::Success,
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            context_updates: HashMap::new(),
            notes: None,
            failure: None,
            usage: None,
            files_touched: Vec::new(),
            jump_to_node: None,
            duration_ms: None,
        }
    }

    /// Create a failed outcome with a deterministic failure class.
    pub fn fail_deterministic(reason: impl Into<String>) -> Self {
        Self {
            status: StageStatus::Fail,
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            context_updates: HashMap::new(),
            notes: None,
            failure: Some(FailureDetail::new(reason, FailureClass::Deterministic)),
            usage: None,
            files_touched: Vec::new(),
            jump_to_node: None,
            duration_ms: None,
        }
    }

    /// Create a failed outcome with the failure class inferred from the message via heuristics.
    pub fn fail_classify(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let failure_class = classify_failure_reason(&reason);
        Self {
            status: StageStatus::Fail,
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            context_updates: HashMap::new(),
            notes: None,
            failure: Some(FailureDetail::new(reason, failure_class)),
            usage: None,
            files_touched: Vec::new(),
            jump_to_node: None,
            duration_ms: None,
        }
    }

    /// Create a retry outcome with the failure class inferred from the message via heuristics.
    pub fn retry_classify(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let failure_class = classify_failure_reason(&reason);
        Self {
            status: StageStatus::Retry,
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            context_updates: HashMap::new(),
            notes: None,
            failure: Some(FailureDetail::new(reason, failure_class)),
            usage: None,
            files_touched: Vec::new(),
            jump_to_node: None,
            duration_ms: None,
        }
    }

    /// Set the failure signature on this outcome. Returns self for chaining.
    #[must_use]
    pub fn with_signature(mut self, sig: Option<impl Into<String>>) -> Self {
        if let Some(ref mut f) = self.failure {
            f.failure_signature = sig.map(Into::into);
        }
        self
    }

    #[must_use]
    pub fn skipped() -> Self {
        Self {
            status: StageStatus::Skipped,
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            context_updates: HashMap::new(),
            notes: None,
            failure: None,
            usage: None,
            files_touched: Vec::new(),
            jump_to_node: None,
            duration_ms: None,
        }
    }

    /// Create a simulated success outcome for dry-run mode.
    #[must_use]
    pub fn simulated(node_id: &str) -> Self {
        Self {
            notes: Some(format!("[Simulated] {node_id}")),
            ..Self::success()
        }
    }

    /// Get the failure reason message, if any.
    pub fn failure_reason(&self) -> Option<&str> {
        self.failure.as_ref().map(|f| f.message.as_str())
    }

    /// Get the failure class, if this is a failed outcome.
    pub fn failure_class(&self) -> Option<FailureClass> {
        self.failure.as_ref().map(|f| f.failure_class)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_status_display() {
        assert_eq!(StageStatus::Success.to_string(), "success");
        assert_eq!(StageStatus::Fail.to_string(), "fail");
        assert_eq!(StageStatus::PartialSuccess.to_string(), "partial_success");
        assert_eq!(StageStatus::Retry.to_string(), "retry");
        assert_eq!(StageStatus::Skipped.to_string(), "skipped");
    }

    #[test]
    fn stage_status_from_str() {
        assert_eq!(
            "success".parse::<StageStatus>().unwrap(),
            StageStatus::Success
        );
        assert_eq!("fail".parse::<StageStatus>().unwrap(), StageStatus::Fail);
        assert_eq!(
            "partial_success".parse::<StageStatus>().unwrap(),
            StageStatus::PartialSuccess
        );
        assert_eq!("retry".parse::<StageStatus>().unwrap(), StageStatus::Retry);
        assert_eq!(
            "skipped".parse::<StageStatus>().unwrap(),
            StageStatus::Skipped
        );
    }

    #[test]
    fn stage_status_from_str_invalid() {
        assert!("unknown".parse::<StageStatus>().is_err());
    }

    #[test]
    fn outcome_success_factory() {
        let o = Outcome::success();
        assert_eq!(o.status, StageStatus::Success);
        assert!(o.preferred_label.is_none());
        assert!(o.suggested_next_ids.is_empty());
        assert!(o.context_updates.is_empty());
        assert!(o.notes.is_none());
        assert!(o.failure.is_none());
    }

    #[test]
    fn outcome_fail_deterministic_factory() {
        let o = Outcome::fail_deterministic("something broke");
        assert_eq!(o.status, StageStatus::Fail);
        assert_eq!(o.failure_reason(), Some("something broke"));
        assert_eq!(o.failure_class(), Some(FailureClass::Deterministic));
    }

    #[test]
    fn outcome_fail_classify_factory() {
        let o = Outcome::fail_classify("connection refused");
        assert_eq!(o.status, StageStatus::Fail);
        assert_eq!(o.failure_reason(), Some("connection refused"));
        assert_eq!(o.failure_class(), Some(FailureClass::TransientInfra));
    }

    #[test]
    fn outcome_retry_classify_factory() {
        let o = Outcome::retry_classify("try again");
        assert_eq!(o.status, StageStatus::Retry);
        assert_eq!(o.failure_reason(), Some("try again"));
    }

    #[test]
    fn outcome_skipped_factory() {
        let o = Outcome::skipped();
        assert_eq!(o.status, StageStatus::Skipped);
        assert!(o.failure.is_none());
    }

    #[test]
    fn failure_detail_construction() {
        let fd = FailureDetail::new("timeout", FailureClass::TransientInfra);
        assert_eq!(fd.message, "timeout");
        assert_eq!(fd.failure_class, FailureClass::TransientInfra);
        assert!(fd.failure_signature.is_none());
    }

    #[test]
    fn failure_detail_serde_roundtrip() {
        let fd = FailureDetail {
            message: "timeout".into(),
            failure_class: FailureClass::TransientInfra,
            failure_signature: Some("sig".into()),
        };
        let json = serde_json::to_string(&fd).unwrap();
        let deserialized: FailureDetail = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.message, "timeout");
        assert_eq!(deserialized.failure_class, FailureClass::TransientInfra);
        assert_eq!(deserialized.failure_signature.as_deref(), Some("sig"));
    }

    #[test]
    fn fail_classify_known_patterns() {
        assert_eq!(
            Outcome::fail_classify("timeout").failure_class(),
            Some(FailureClass::TransientInfra)
        );
        assert_eq!(
            Outcome::fail_classify("context length exceeded").failure_class(),
            Some(FailureClass::BudgetExhausted)
        );
        assert_eq!(
            Outcome::fail_classify("cancel").failure_class(),
            Some(FailureClass::Canceled)
        );
    }

    #[test]
    fn failure_field_is_some_for_failures() {
        assert!(Outcome::fail_deterministic("x").failure.is_some());
    }

    #[test]
    fn failure_field_is_none_for_success() {
        assert!(Outcome::success().failure.is_none());
    }

    #[test]
    fn with_signature_builder() {
        let o = Outcome::fail_deterministic("x").with_signature(Some("sig"));
        assert_eq!(
            o.failure.as_ref().unwrap().failure_signature.as_deref(),
            Some("sig")
        );
    }

    #[test]
    fn stage_usage_serialization_with_cache_and_reasoning() {
        let usage = StageUsage {
            model: "claude-opus-4-6".to_string(),
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: Some(800),
            cache_write_tokens: Some(50),
            reasoning_tokens: Some(100),
            speed: None,
            cost: None,
        };
        let json = serde_json::to_string(&usage).unwrap();
        assert!(json.contains("\"cache_read_tokens\":800"));
        assert!(json.contains("\"reasoning_tokens\":100"));

        let deserialized: StageUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.cache_read_tokens, Some(800));
        assert_eq!(deserialized.reasoning_tokens, Some(100));
    }

    #[test]
    fn stage_usage_serialization_omits_none_optional_fields() {
        let usage = StageUsage {
            model: "test-model".to_string(),
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            speed: None,
            cost: None,
        };
        let json = serde_json::to_string(&usage).unwrap();
        assert!(!json.contains("cache_read_tokens"));
        assert!(!json.contains("reasoning_tokens"));
    }

    #[test]
    fn outcome_files_touched_serialization() {
        let mut o = Outcome::success();
        o.files_touched = vec!["src/main.rs".to_string(), "README.md".to_string()];
        let json = serde_json::to_string(&o).unwrap();
        assert!(json.contains("files_touched"));
        assert!(json.contains("src/main.rs"));

        let deserialized: Outcome = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.files_touched.len(), 2);
    }

    #[test]
    fn outcome_empty_files_touched_omitted() {
        let o = Outcome::success();
        let json = serde_json::to_string(&o).unwrap();
        assert!(!json.contains("files_touched"));
    }

    #[test]
    fn outcome_serialization_roundtrip() {
        let mut o = Outcome::success();
        o.notes = Some("done".to_string());
        o.context_updates
            .insert("key".to_string(), serde_json::json!("val"));

        let json = serde_json::to_string(&o).unwrap();
        let deserialized: Outcome = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.status, StageStatus::Success);
        assert_eq!(deserialized.notes.as_deref(), Some("done"));
        assert_eq!(
            deserialized.context_updates.get("key"),
            Some(&serde_json::json!("val"))
        );
    }

    #[test]
    fn stage_status_serde_roundtrip() {
        let json = serde_json::to_string(&StageStatus::PartialSuccess).unwrap();
        assert_eq!(json, "\"partial_success\"");
        let parsed: StageStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, StageStatus::PartialSuccess);
    }

    #[test]
    fn outcome_simulated_factory() {
        let o = Outcome::simulated("my_node");
        assert_eq!(o.status, StageStatus::Success);
        assert_eq!(o.notes.as_deref(), Some("[Simulated] my_node"));
        assert!(o.failure.is_none());
        assert!(o.context_updates.is_empty());
    }
}
