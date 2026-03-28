pub use fabro_core::outcome::{FailureCategory, FailureDetail, OutcomeMeta, StageStatus};
pub use fabro_types::usage::StageUsage;

use crate::error::classify_failure_reason;
use fabro_llm::types::Usage as LlmUsage;

pub fn stage_usage_to_llm(u: &StageUsage) -> LlmUsage {
    LlmUsage {
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

pub type Outcome = fabro_core::Outcome<Option<StageUsage>>;

pub trait OutcomeExt: Sized {
    fn fail_deterministic(reason: impl Into<String>) -> Self;
    fn fail_classify(reason: impl Into<String>) -> Self;
    fn retry_classify(reason: impl Into<String>) -> Self;
    fn simulated(node_id: &str) -> Self;
    fn with_signature(self, sig: Option<impl Into<String>>) -> Self;
    fn failure_reason(&self) -> Option<&str>;
    fn failure_category(&self) -> Option<FailureCategory>;
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

#[must_use]
pub fn format_cost(cost: f64) -> String {
    format!("${cost:.2}")
}
