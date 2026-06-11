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
/// pricing for the model. Returns `None` if the catalog is absent, the
/// model is not in the catalog, or the model has no pricing.
#[must_use]
pub(crate) fn estimate_cost_usd(
    catalog: Option<&Catalog>,
    provider: &str,
    model: &str,
    tokens: &TokenCounts,
    speed: Option<Speed>,
) -> Option<f64> {
    let catalog = catalog?;
    // The billing machinery compares ModelRefs against the catalog's
    // canonical identity, so resolve model aliases and provider names first.
    let model = catalog.get(model)?;
    let provider = catalog.provider(&ProviderId::new(provider))?;
    let model_ref = ModelRef {
        provider: provider.id.clone(),
        model_id: model.id.clone(),
        speed,
    };
    let micros = catalog.price_tokens(&model_ref, tokens)?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "micros fit comfortably in f64 for any realistic completion cost"
    )]
    Some(micros as f64 / 1_000_000.0)
}

/// Stamp a catalog-estimated cost onto `response` unless the provider
/// already supplied one (providers that return authoritative billing data
/// in-band set [`CostSource::Authoritative`] directly and take precedence).
/// `model` is the request's model id or alias (the catalog lookup resolves
/// aliases); the response's provider name selects the billing policy.
pub(crate) fn apply_estimated_cost(
    catalog: Option<&Catalog>,
    model: &str,
    speed: Option<Speed>,
    response: &mut Response,
) {
    if response.cost_usd.is_some() {
        return;
    }
    let estimate = estimate_cost_usd(catalog, &response.provider, model, &response.usage, speed);
    response.cost_usd = estimate;
    response.cost_source = estimate.map(|_| CostSource::Estimated);
}

#[cfg(test)]
mod tests {
    use fabro_model::catalog::LlmCatalogSettings;

    use super::*;
    use crate::types::{FinishReason, Message};

    /// Single-provider catalog with one `gpt-test` model (alias `gpt-alias`)
    /// and the given `[models."gpt-test".costs]` block (empty for unpriced).
    fn test_catalog(costs_block: &str) -> Catalog {
        let toml = format!(
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

{costs_block}
"#
        );
        let settings: LlmCatalogSettings = toml::from_str(&toml).unwrap();
        Catalog::from_settings(&settings).unwrap()
    }

    fn priced_catalog(input_cost_per_mtok: f64, output_cost_per_mtok: f64) -> Catalog {
        test_catalog(&format!(
            r#"
[models."gpt-test".costs]
input_cost_per_mtok = {input_cost_per_mtok}
output_cost_per_mtok = {output_cost_per_mtok}
"#
        ))
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
        assert_eq!(
            estimate_cost_usd(None, "openai", "gpt-test", &tokens, None),
            None
        );
    }

    #[test]
    fn returns_estimated_when_model_priced() {
        let catalog = priced_catalog(1.0, 2.0);
        let tokens = TokenCounts {
            input_tokens: 1_000_000, // 1M tokens at $1/Mtok = $1.00
            output_tokens: 500_000,  // 500k tokens at $2/Mtok = $1.00
            ..TokenCounts::default()
        };
        let cost = estimate_cost_usd(Some(&catalog), "openai", "gpt-test", &tokens, None)
            .expect("cost should be Some");
        assert!((cost - 2.0).abs() < 1e-9, "expected ~$2.00, got {cost}");
    }

    #[test]
    fn resolves_model_aliases() {
        let catalog = priced_catalog(1.0, 2.0);
        let tokens = TokenCounts {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..TokenCounts::default()
        };
        let cost = estimate_cost_usd(Some(&catalog), "openai", "gpt-alias", &tokens, None);
        assert!(cost.is_some());
    }

    #[test]
    fn returns_none_when_model_missing_from_catalog() {
        let catalog = priced_catalog(1.0, 2.0);
        let tokens = TokenCounts {
            input_tokens: 1000,
            output_tokens: 500,
            ..TokenCounts::default()
        };
        let cost = estimate_cost_usd(Some(&catalog), "openai", "nonexistent-model", &tokens, None);
        assert_eq!(cost, None);
    }

    #[test]
    fn returns_none_when_model_has_no_pricing() {
        let catalog = test_catalog("");
        let tokens = TokenCounts {
            input_tokens: 1000,
            output_tokens: 500,
            ..TokenCounts::default()
        };
        let cost = estimate_cost_usd(Some(&catalog), "openai", "gpt-test", &tokens, None);
        assert_eq!(cost, None);
    }

    #[test]
    fn micros_to_usd_conversion_is_exact_for_integer_amounts() {
        // input_cost_per_mtok = 1.5 USD; 1M input tokens with no output
        // yields exactly 1_500_000 micros = $1.50 (representable as f64).
        let catalog = priced_catalog(1.5, 0.0);
        let tokens = TokenCounts {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..TokenCounts::default()
        };
        let cost = estimate_cost_usd(Some(&catalog), "openai", "gpt-test", &tokens, None)
            .expect("cost should be Some");
        assert!(
            (cost - 1.5).abs() < f64::EPSILON,
            "expected $1.50 exact, got {cost}"
        );
    }

    #[test]
    fn apply_estimated_cost_stamps_estimate() {
        let catalog = priced_catalog(1.0, 2.0);
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
    fn apply_estimated_cost_leaves_source_unset_without_estimate() {
        let mut response = response_with_usage(TokenCounts::default());

        apply_estimated_cost(None, "gpt-test", None, &mut response);

        assert_eq!(response.cost_usd, None);
        assert_eq!(response.cost_source, None);
    }

    #[test]
    fn apply_estimated_cost_keeps_existing_cost() {
        let catalog = priced_catalog(1.0, 2.0);
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
