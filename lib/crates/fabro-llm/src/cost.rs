//! Catalog-derived cost estimation for completion responses.
//!
//! The estimate is a thin wrapper over the catalog's billing machinery
//! ([`Catalog::price_tokens`]), which is billing-policy- and speed-aware.
//! Costs are stamped onto responses by the [`Client`](crate::Client) as a
//! post-decode step, so codecs stay wire-translation-only and every
//! registered adapter (including custom ones) gets the same treatment.

use fabro_model::billing::{ModelRef, Speed, TokenCounts};
use fabro_model::{Catalog, ProviderId};

use crate::types::{CostSource, Response};

/// Estimate the USD cost of a completion from the catalog's per-token
/// pricing for the model. Returns `(None, None)` if the catalog is absent,
/// the model is not in the catalog, or the model has no pricing.
///
/// Providers that return authoritative billing data in-band bypass this and
/// set [`CostSource::Authoritative`] directly.
#[must_use]
pub fn estimate_cost_usd(
    catalog: Option<&Catalog>,
    provider: &str,
    model: &str,
    tokens: &TokenCounts,
    speed: Option<Speed>,
) -> (Option<f64>, Option<CostSource>) {
    let Some(catalog) = catalog else {
        return (None, None);
    };
    // The billing machinery compares ModelRefs against the catalog's
    // canonical identity, so resolve model aliases and provider names first.
    let Some(model) = catalog.get(model) else {
        return (None, None);
    };
    let Some(provider) = catalog.provider(&ProviderId::new(provider)) else {
        return (None, None);
    };
    let model_ref = ModelRef {
        provider: provider.id.clone(),
        model_id: model.id.clone(),
        speed,
    };
    let Some(micros) = catalog.price_tokens(&model_ref, tokens) else {
        return (None, None);
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "micros fit comfortably in f64 for any realistic completion cost"
    )]
    let usd = micros as f64 / 1_000_000.0;
    (Some(usd), Some(CostSource::Estimated))
}

/// Stamp a catalog-estimated cost onto `response` unless the provider
/// already supplied one. `model` is the request's model id or alias (the
/// catalog lookup resolves aliases); the response's provider name selects
/// the billing policy.
pub(crate) fn apply_estimated_cost(
    catalog: Option<&Catalog>,
    model: &str,
    speed: Option<Speed>,
    response: &mut Response,
) {
    if response.cost_usd.is_some() {
        return;
    }
    let (cost_usd, cost_source) =
        estimate_cost_usd(catalog, &response.provider, model, &response.usage, speed);
    response.cost_usd = cost_usd;
    response.cost_source = cost_source;
}

#[cfg(test)]
mod tests {
    use fabro_model::catalog::LlmCatalogSettings;

    use super::*;
    use crate::types::{FinishReason, Message};

    fn catalog_with_openai_model() -> Catalog {
        let settings: LlmCatalogSettings = toml::from_str(
            r#"
[providers.openai]
display_name = "OpenAI"
adapter = "openai"
agent_profile = "openai"

[models."gpt-test"]
provider = "openai"
display_name = "GPT Test"
family = "gpt"
default = true
aliases = ["gpt-alias"]

[models."gpt-test".limits]
context_window = 200000
max_output = 4096

[models."gpt-test".features]
tools = true
vision = false
reasoning = false

[models."gpt-test".costs]
input_cost_per_mtok = 1.0
output_cost_per_mtok = 2.0
"#,
        )
        .unwrap();
        Catalog::from_settings(&settings).unwrap()
    }

    fn catalog_without_costs() -> Catalog {
        let settings: LlmCatalogSettings = toml::from_str(
            r#"
[providers.openai]
display_name = "OpenAI"
adapter = "openai"
agent_profile = "openai"

[models."gpt-no-cost"]
provider = "openai"
display_name = "GPT No Cost"
family = "gpt"
default = true

[models."gpt-no-cost".limits]
context_window = 200000
max_output = 4096

[models."gpt-no-cost".features]
tools = true
vision = false
reasoning = false
"#,
        )
        .unwrap();
        Catalog::from_settings(&settings).unwrap()
    }

    fn response_with_usage(tokens: TokenCounts) -> Response {
        Response {
            id:            "resp".to_string(),
            model:         "gpt-test".to_string(),
            provider:      "openai".to_string(),
            message:       Message::assistant("hi"),
            finish_reason: FinishReason::Stop,
            usage:         tokens,
            raw:           None,
            warnings:      vec![],
            rate_limit:    None,
            cost_usd:      None,
            cost_source:   None,
        }
    }

    #[test]
    fn returns_none_when_catalog_is_none() {
        let tokens = TokenCounts {
            input_tokens: 1000,
            output_tokens: 500,
            ..TokenCounts::default()
        };
        let (cost, source) = estimate_cost_usd(None, "openai", "gpt-test", &tokens, None);
        assert_eq!(cost, None);
        assert_eq!(source, None);
    }

    #[test]
    fn returns_estimated_when_model_priced() {
        let catalog = catalog_with_openai_model();
        let tokens = TokenCounts {
            input_tokens: 1_000_000, // 1M tokens at $1/Mtok = $1.00
            output_tokens: 500_000,  // 500k tokens at $2/Mtok = $1.00
            ..TokenCounts::default()
        };
        let (cost, source) = estimate_cost_usd(Some(&catalog), "openai", "gpt-test", &tokens, None);
        assert_eq!(source, Some(CostSource::Estimated));
        let cost = cost.expect("cost should be Some");
        assert!((cost - 2.0).abs() < 1e-9, "expected ~$2.00, got {cost}");
    }

    #[test]
    fn resolves_model_aliases() {
        let catalog = catalog_with_openai_model();
        let tokens = TokenCounts {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..TokenCounts::default()
        };
        let (cost, source) =
            estimate_cost_usd(Some(&catalog), "openai", "gpt-alias", &tokens, None);
        assert_eq!(source, Some(CostSource::Estimated));
        assert!(cost.is_some());
    }

    #[test]
    fn returns_none_when_model_missing_from_catalog() {
        let catalog = catalog_with_openai_model();
        let tokens = TokenCounts {
            input_tokens: 1000,
            output_tokens: 500,
            ..TokenCounts::default()
        };
        let (cost, source) =
            estimate_cost_usd(Some(&catalog), "openai", "nonexistent-model", &tokens, None);
        assert_eq!(cost, None);
        assert_eq!(source, None);
    }

    #[test]
    fn returns_none_when_model_has_no_pricing() {
        let catalog = catalog_without_costs();
        let tokens = TokenCounts {
            input_tokens: 1000,
            output_tokens: 500,
            ..TokenCounts::default()
        };
        let (cost, source) =
            estimate_cost_usd(Some(&catalog), "openai", "gpt-no-cost", &tokens, None);
        assert_eq!(cost, None);
        assert_eq!(source, None);
    }

    #[test]
    fn micros_to_usd_conversion_is_exact_for_integer_amounts() {
        // input_cost_per_mtok = 1.5 USD; 1M input tokens with no output
        // yields exactly 1_500_000 micros = $1.50 (representable as f64).
        let settings: LlmCatalogSettings = toml::from_str(
            r#"
[providers.openai]
display_name = "OpenAI"
adapter = "openai"
agent_profile = "openai"

[models."gpt-exact"]
provider = "openai"
display_name = "GPT Exact"
family = "gpt"
default = true

[models."gpt-exact".limits]
context_window = 200000
max_output = 4096

[models."gpt-exact".features]
tools = true
vision = false
reasoning = false

[models."gpt-exact".costs]
input_cost_per_mtok = 1.5
output_cost_per_mtok = 0.0
"#,
        )
        .unwrap();
        let catalog = Catalog::from_settings(&settings).unwrap();
        let tokens = TokenCounts {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..TokenCounts::default()
        };
        let (cost, _source) =
            estimate_cost_usd(Some(&catalog), "openai", "gpt-exact", &tokens, None);
        let cost = cost.expect("cost should be Some");
        assert!(
            (cost - 1.5).abs() < f64::EPSILON,
            "expected $1.50 exact, got {cost}"
        );
    }

    #[test]
    fn apply_estimated_cost_stamps_estimate() {
        let catalog = catalog_with_openai_model();
        let mut response = response_with_usage(TokenCounts {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..TokenCounts::default()
        });

        apply_estimated_cost(Some(&catalog), "gpt-test", None, &mut response);

        assert_eq!(response.cost_source, Some(CostSource::Estimated));
        assert!(response.cost_usd.is_some());
    }

    #[test]
    fn apply_estimated_cost_keeps_existing_cost() {
        let catalog = catalog_with_openai_model();
        let mut response = response_with_usage(TokenCounts {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..TokenCounts::default()
        });
        response.cost_usd = Some(0.42);
        response.cost_source = Some(CostSource::Authoritative);

        apply_estimated_cost(Some(&catalog), "gpt-test", None, &mut response);

        assert_eq!(response.cost_usd, Some(0.42));
        assert_eq!(response.cost_source, Some(CostSource::Authoritative));
    }
}
