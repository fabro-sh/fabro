use serde::{Deserialize, Serialize};

pub use fabro_core::outcome::{FailureCategory, FailureDetail, OutcomeMeta, StageStatus};

use crate::error::classify_failure_reason;

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

/// The workflow-specific Outcome type, parameterized with optional stage usage.
pub type Outcome = fabro_core::Outcome<Option<StageUsage>>;

/// Extension trait for workflow-specific Outcome factory methods and accessors.
pub trait OutcomeExt: Sized {
    /// Create a failed outcome with a deterministic failure category.
    fn fail_deterministic(reason: impl Into<String>) -> Self;

    /// Create a failed outcome with the failure category inferred from the message via heuristics.
    fn fail_classify(reason: impl Into<String>) -> Self;

    /// Create a retry outcome with the failure category inferred from the message via heuristics.
    fn retry_classify(reason: impl Into<String>) -> Self;

    /// Create a simulated success outcome for dry-run mode.
    fn simulated(node_id: &str) -> Self;

    /// Set the failure signature on this outcome. Returns self for chaining.
    fn with_signature(self, sig: Option<impl Into<String>>) -> Self;

    /// Get the failure reason message, if any.
    fn failure_reason(&self) -> Option<&str>;

    /// Get the failure category, if this is a failed outcome.
    fn failure_category(&self) -> Option<FailureCategory>;

    /// Resolve the effective failure category for this outcome.
    ///
    /// Returns `None` for success, partial success, and skipped outcomes.
    /// Failed and retry outcomes default to `Deterministic` when no
    /// structured failure category is present.
    fn classified_failure_category(&self) -> Option<FailureCategory>;
}

impl OutcomeExt for Outcome {
    fn fail_deterministic(reason: impl Into<String>) -> Self {
        Self {
            status: StageStatus::Fail,
            failure: Some(FailureDetail::new(reason, FailureCategory::Deterministic)),
            ..Self::default()
        }
    }

    fn fail_classify(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let category = classify_failure_reason(&reason);
        Self {
            status: StageStatus::Fail,
            failure: Some(FailureDetail::new(reason, category)),
            ..Self::default()
        }
    }

    fn retry_classify(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let category = classify_failure_reason(&reason);
        Self {
            status: StageStatus::Retry,
            failure: Some(FailureDetail::new(reason, category)),
            ..Self::default()
        }
    }

    fn simulated(node_id: &str) -> Self {
        Self {
            notes: Some(format!("[Simulated] {node_id}")),
            ..Self::success()
        }
    }

    fn with_signature(mut self, sig: Option<impl Into<String>>) -> Self {
        if let Some(ref mut f) = self.failure {
            f.signature = sig.map(Into::into);
        }
        self
    }

    fn failure_reason(&self) -> Option<&str> {
        self.failure.as_ref().map(|f| f.message.as_str())
    }

    fn failure_category(&self) -> Option<FailureCategory> {
        self.failure.as_ref().map(|f| f.category)
    }

    fn classified_failure_category(&self) -> Option<FailureCategory> {
        match self.status {
            StageStatus::Success | StageStatus::PartialSuccess | StageStatus::Skipped => None,
            StageStatus::Fail | StageStatus::Retry => self
                .failure_category()
                .or(Some(FailureCategory::Deterministic)),
        }
    }
}

/// Compute the dollar cost for a stage's token usage, if pricing is available.
#[must_use]
pub fn compute_stage_cost(usage: &StageUsage) -> Option<f64> {
    let info = fabro_model::Catalog::builtin().get(&usage.model)?;
    let input_rate = info.costs.input_cost_per_mtok?;
    let output_rate = info.costs.output_cost_per_mtok?;
    let multiplier = if usage.speed.as_deref() == Some("fast") {
        6.0
    } else {
        1.0
    };
    Some(
        (usage.input_tokens as f64 * input_rate / 1_000_000.0
            + usage.output_tokens as f64 * output_rate / 1_000_000.0)
            * multiplier,
    )
}

/// Format a dollar cost for display (e.g. `"$1.23"`).
#[must_use]
pub fn format_cost(cost: f64) -> String {
    format!("${cost:.2}")
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
        assert_eq!(o.failure_category(), Some(FailureCategory::Deterministic));
    }

    #[test]
    fn outcome_fail_classify_factory() {
        let o = Outcome::fail_classify("connection refused");
        assert_eq!(o.status, StageStatus::Fail);
        assert_eq!(o.failure_reason(), Some("connection refused"));
        assert_eq!(o.failure_category(), Some(FailureCategory::TransientInfra));
    }

    #[test]
    fn outcome_retry_classify_factory() {
        let o = Outcome::retry_classify("try again");
        assert_eq!(o.status, StageStatus::Retry);
        assert_eq!(o.failure_reason(), Some("try again"));
    }

    #[test]
    fn outcome_skipped_factory() {
        let o = Outcome::skipped("");
        assert_eq!(o.status, StageStatus::Skipped);
        assert!(o.failure.is_none());
    }

    #[test]
    fn failure_detail_construction() {
        let fd = FailureDetail::new("timeout", FailureCategory::TransientInfra);
        assert_eq!(fd.message, "timeout");
        assert_eq!(fd.category, FailureCategory::TransientInfra);
        assert!(fd.signature.is_none());
    }

    #[test]
    fn failure_detail_serde_roundtrip() {
        let fd = FailureDetail {
            message: "timeout".into(),
            category: FailureCategory::TransientInfra,
            signature: Some("sig".into()),
        };
        let json = serde_json::to_string(&fd).unwrap();
        let deserialized: FailureDetail = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.message, "timeout");
        assert_eq!(deserialized.category, FailureCategory::TransientInfra);
        assert_eq!(deserialized.signature.as_deref(), Some("sig"));
    }

    #[test]
    fn fail_classify_known_patterns() {
        assert_eq!(
            Outcome::fail_classify("timeout").failure_category(),
            Some(FailureCategory::TransientInfra)
        );
        assert_eq!(
            Outcome::fail_classify("context length exceeded").failure_category(),
            Some(FailureCategory::BudgetExhausted)
        );
        assert_eq!(
            Outcome::fail_classify("cancel").failure_category(),
            Some(FailureCategory::Canceled)
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
            o.failure.as_ref().unwrap().signature.as_deref(),
            Some("sig")
        );
    }

    #[test]
    fn classified_failure_category_returns_none_for_success() {
        assert!(Outcome::success().classified_failure_category().is_none());
    }

    #[test]
    fn classified_failure_category_returns_none_for_skipped() {
        assert!(Outcome::skipped("").classified_failure_category().is_none());
    }

    #[test]
    fn classified_failure_category_returns_none_for_partial_success() {
        let outcome = Outcome {
            status: StageStatus::PartialSuccess,
            ..Outcome::success()
        };
        assert!(outcome.classified_failure_category().is_none());
    }

    #[test]
    fn classified_failure_category_reads_failure_detail() {
        let mut outcome = Outcome::fail_classify("some error");
        outcome.failure.as_mut().unwrap().category = FailureCategory::BudgetExhausted;
        assert_eq!(
            outcome.classified_failure_category(),
            Some(FailureCategory::BudgetExhausted)
        );
    }

    #[test]
    fn classified_failure_category_uses_failure_reason_heuristics() {
        let outcome = Outcome::fail_classify("rate limited by provider");
        assert_eq!(
            outcome.classified_failure_category(),
            Some(FailureCategory::TransientInfra)
        );
    }

    #[test]
    fn classified_failure_category_defaults_to_deterministic() {
        let outcome = Outcome::fail_classify("something went wrong");
        assert_eq!(
            outcome.classified_failure_category(),
            Some(FailureCategory::Deterministic)
        );
    }

    #[test]
    fn classified_failure_category_fail_no_reason_is_deterministic() {
        let outcome = Outcome {
            status: StageStatus::Fail,
            failure: None,
            ..Outcome::success()
        };
        assert_eq!(
            outcome.classified_failure_category(),
            Some(FailureCategory::Deterministic)
        );
    }

    #[test]
    fn classified_failure_category_retry_status_uses_heuristics() {
        let outcome = Outcome::retry_classify("connection refused");
        assert_eq!(
            outcome.classified_failure_category(),
            Some(FailureCategory::TransientInfra)
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

    #[test]
    fn format_cost_zero() {
        assert_eq!(format_cost(0.0), "$0.00");
    }

    #[test]
    fn format_cost_normal() {
        assert_eq!(format_cost(1.5), "$1.50");
    }

    #[test]
    fn format_cost_rounds() {
        assert_eq!(format_cost(123.456), "$123.46");
    }

    #[test]
    fn compute_stage_cost_known_model() {
        let usage = StageUsage {
            model: "claude-sonnet-4-5".into(),
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            speed: None,
            cost: None,
        };
        let cost = compute_stage_cost(&usage);
        assert!(cost.is_some());
        assert!(cost.unwrap() > 0.0);
    }

    #[test]
    fn compute_stage_cost_unknown_model() {
        let usage = StageUsage {
            model: "nonexistent-model-xyz".into(),
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            speed: None,
            cost: None,
        };
        assert_eq!(compute_stage_cost(&usage), None);
    }

    #[test]
    fn compute_stage_cost_fast_mode_6x_multiplier() {
        let standard_usage = StageUsage {
            model: "claude-sonnet-4-5".into(),
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            speed: None,
            cost: None,
        };
        let fast_usage = StageUsage {
            model: "claude-sonnet-4-5".into(),
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            speed: Some("fast".into()),
            cost: None,
        };
        let standard_cost = compute_stage_cost(&standard_usage).unwrap();
        let fast_cost = compute_stage_cost(&fast_usage).unwrap();
        assert!(
            (fast_cost - standard_cost * 6.0).abs() < 1e-10,
            "fast mode should be 6x standard cost: standard={standard_cost}, fast={fast_cost}"
        );
    }
}
