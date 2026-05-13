pub use fabro_core::outcome::{
    FailureCategory, FailureDetail, OutcomeMeta, StageOutcome, StageState,
};
use fabro_llm::types::TokenCounts as LlmTokenCounts;
use fabro_model::{
    Catalog, ModelBillingFacts, ModelBillingInput, ModelRef, ModelUsage, TokenCounts,
};
pub use fabro_types::BilledModelUsage;

use crate::error::classify_failure_reason;

pub type Outcome = fabro_core::Outcome<Option<BilledModelUsage>>;

#[must_use]
pub fn billed_model_usage_from_llm(
    catalog: &Catalog,
    model: &ModelRef,
    usage: &LlmTokenCounts,
) -> BilledModelUsage {
    let tokens = token_counts_from_llm_usage(usage);
    let facts = catalog.billing_facts_for(model, &tokens);
    let input = ModelBillingInput {
        usage: ModelUsage {
            model: model.clone(),
            tokens,
        },
        facts,
    };

    let total_usd_micros = catalog
        .pricing_for(model)
        .and_then(|pricing| pricing.bill(&input))
        .map(|amount| amount.0);

    BilledModelUsage {
        input,
        total_usd_micros,
    }
}

#[must_use]
pub fn unpriced_model_usage_from_llm(model: ModelRef, usage: &LlmTokenCounts) -> BilledModelUsage {
    let tokens = token_counts_from_llm_usage(usage);
    let facts = ModelBillingFacts::for_provider_id(&model.provider, &tokens);
    BilledModelUsage {
        input:            ModelBillingInput {
            usage: ModelUsage { model, tokens },
            facts,
        },
        total_usd_micros: None,
    }
}

pub trait OutcomeExt: Sized {
    fn fail_deterministic(reason: impl Into<String>) -> Self;
    fn fail_classify(reason: impl Into<String>) -> Self;
    fn retry_classify(reason: impl Into<String>) -> Self;
    fn simulated(node_id: &str) -> Self;
    #[must_use]
    fn with_signature(self, sig: Option<impl Into<String>>) -> Self;
    fn failure_reason(&self) -> Option<&str>;
    fn failure_category(&self) -> Option<FailureCategory>;
    fn classified_failure_category(&self) -> Option<FailureCategory>;
}

impl OutcomeExt for Outcome {
    fn fail_deterministic(reason: impl Into<String>) -> Self {
        Self {
            status: StageOutcome::Failed {
                retry_requested: false,
            },
            failure: Some(FailureDetail::new(reason, FailureCategory::Deterministic)),
            ..Self::default()
        }
    }

    fn fail_classify(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let category = classify_failure_reason(&reason);
        Self {
            status: StageOutcome::Failed {
                retry_requested: false,
            },
            failure: Some(FailureDetail::new(reason, category)),
            ..Self::default()
        }
    }

    fn retry_classify(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let category = classify_failure_reason(&reason);
        Self {
            status: StageOutcome::Failed {
                retry_requested: true,
            },
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
        if let Some(ref mut failure) = self.failure {
            failure.signature = sig.map(Into::into);
        }
        self
    }

    fn failure_reason(&self) -> Option<&str> {
        self.failure
            .as_ref()
            .map(|failure| failure.message.as_str())
    }

    fn failure_category(&self) -> Option<FailureCategory> {
        self.failure.as_ref().map(|failure| failure.category)
    }

    fn classified_failure_category(&self) -> Option<FailureCategory> {
        match self.status {
            StageOutcome::Succeeded | StageOutcome::PartiallySucceeded | StageOutcome::Skipped => {
                None
            }
            StageOutcome::Failed { .. } => self
                .failure_category()
                .or(Some(FailureCategory::Deterministic)),
        }
    }
}

#[must_use]
pub fn format_cost(cost: f64) -> String {
    format!("${cost:.2}")
}

fn token_counts_from_llm_usage(usage: &LlmTokenCounts) -> TokenCounts {
    usage.clone()
}

#[cfg(test)]
mod tests {
    use fabro_llm::types::TokenCounts;
    use fabro_model::catalog::LlmCatalogSettings;
    use fabro_model::{Catalog, ModelRef, Provider, ProviderId, Speed};

    use super::{OutcomeExt, billed_model_usage_from_llm};

    fn model_ref(provider: ProviderId, model_id: &str, speed: Option<Speed>) -> ModelRef {
        ModelRef {
            provider,
            model_id: model_id.to_string(),
            speed,
        }
    }

    #[test]
    fn billed_model_usage_from_llm_bills_openai_cached_input_and_reasoning_output() {
        let usage = TokenCounts {
            input_tokens: 500_000,
            output_tokens: 125_000,
            reasoning_tokens: 25_000,
            cache_read_tokens: 250_000,
            ..TokenCounts::default()
        };
        let billed = billed_model_usage_from_llm(
            Catalog::builtin(),
            &model_ref(Provider::OpenAi.id(), "gpt-5.4", None),
            &usage,
        );

        assert_eq!(billed.total_usd_micros, Some(3_562_500));
        assert_eq!(billed.tokens().output_tokens, 125_000);
        assert_eq!(billed.tokens().reasoning_tokens, 25_000);
    }

    #[test]
    fn retry_classify_marks_failed_outcome_with_retry_request() {
        let outcome = crate::outcome::Outcome::retry_classify("timeout");

        assert_eq!(outcome.status, crate::outcome::StageOutcome::Failed {
            retry_requested: true,
        });
        assert!(outcome.status.retry_requested());
    }

    #[test]
    fn billed_model_usage_from_llm_bills_anthropic_fast_mode_cache_write_pricing() {
        let usage = TokenCounts {
            input_tokens:       100_000,
            output_tokens:      10_000,
            reasoning_tokens:   5_000,
            cache_read_tokens:  20_000,
            cache_write_tokens: 30_000,
        };
        let billed = billed_model_usage_from_llm(
            Catalog::builtin(),
            &model_ref(
                Provider::Anthropic.id(),
                "claude-opus-4-6",
                Some(Speed::Fast),
            ),
            &usage,
        );

        assert_eq!(billed.total_usd_micros, Some(6_435_000));
    }

    #[test]
    fn billed_model_usage_from_llm_uses_injected_custom_catalog() {
        let settings: LlmCatalogSettings = toml::from_str(
            r#"
[providers.proxy]
display_name = "Proxy"
adapter = "openai_compatible"
base_url = "https://proxy.example/v1"

[models.canonical-model]
provider = "proxy"
api_id = "wire-model"
display_name = "Canonical Model"
family = "proxy"
default = true

[models.canonical-model.limits]
context_window = 1000

[models.canonical-model.features]
tools = true
vision = false
reasoning = false

[models.canonical-model.costs]
input_cost_per_mtok = 1.0
output_cost_per_mtok = 2.0
"#,
        )
        .unwrap();
        let catalog = Catalog::from_settings(&settings).unwrap();
        let usage = TokenCounts {
            input_tokens: 500_000,
            output_tokens: 250_000,
            ..TokenCounts::default()
        };

        let billed = billed_model_usage_from_llm(
            &catalog,
            &model_ref(ProviderId::new("proxy"), "canonical-model", None),
            &usage,
        );

        assert_eq!(&billed.model().provider, &ProviderId::new("proxy"));
        assert_eq!(billed.model_id(), "canonical-model");
        assert_eq!(billed.total_usd_micros, Some(1_000_000));
    }

    #[test]
    fn billed_model_usage_from_llm_does_not_bill_provider_api_id() {
        let settings: LlmCatalogSettings = toml::from_str(
            r#"
[providers.proxy]
display_name = "Proxy"
adapter = "openai_compatible"
base_url = "https://proxy.example/v1"

[models.canonical-model]
provider = "proxy"
api_id = "wire-model"
display_name = "Canonical Model"
family = "proxy"
default = true

[models.canonical-model.limits]
context_window = 1000

[models.canonical-model.features]
tools = true
vision = false
reasoning = false

[models.canonical-model.costs]
input_cost_per_mtok = 1.0
output_cost_per_mtok = 2.0
"#,
        )
        .unwrap();
        let catalog = Catalog::from_settings(&settings).unwrap();

        let billed = billed_model_usage_from_llm(
            &catalog,
            &model_ref(ProviderId::new("proxy"), "wire-model", None),
            &TokenCounts {
                input_tokens: 500_000,
                output_tokens: 250_000,
                ..TokenCounts::default()
            },
        );

        assert_eq!(billed.model_id(), "wire-model");
        assert_eq!(billed.total_usd_micros, None);
    }

    #[test]
    fn billed_model_usage_round_trips_dense_token_counts() {
        let usage = TokenCounts {
            input_tokens:       100,
            output_tokens:      40,
            reasoning_tokens:   5,
            cache_read_tokens:  20,
            cache_write_tokens: 10,
        };
        let billed = billed_model_usage_from_llm(
            Catalog::builtin(),
            &model_ref(Provider::Anthropic.id(), "claude-opus-4-6", None),
            &usage,
        );

        assert_eq!(billed.tokens().clone(), usage);
    }
}
