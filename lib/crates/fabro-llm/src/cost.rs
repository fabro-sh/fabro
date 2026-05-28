//! Catalog-derived cost estimation for completion responses.

use fabro_model::billing::{ModelRef, Speed, TokenCounts};
use fabro_model::{Catalog, ProviderId};

use crate::types::CostSource;

/// Estimate the USD cost of a completion from the catalog's per-token
/// pricing for the model. Returns `(None, None)` if the catalog is absent,
/// the model is not in the catalog, or the model has no pricing.
///
/// Used by adapters that don't receive an authoritative cost from the
/// provider. Adapters with authoritative pricing (e.g. OpenRouter
/// `usage.cost`) bypass this and set `Authoritative` directly.
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
    let model_ref = ModelRef {
        provider: ProviderId::from(provider),
        model_id: model.to_string(),
        speed,
    };
    let Some(micros) = catalog.price_tokens(&model_ref, tokens) else {
        return (None, None);
    };
    #[allow(
        clippy::cast_precision_loss,
        reason = "micros fit comfortably in f64 for any realistic completion cost"
    )]
    let usd = micros as f64 / 1_000_000.0;
    (Some(usd), Some(CostSource::Estimated))
}

#[cfg(test)]
mod tests {
    use fabro_model::catalog::LlmCatalogSettings;

    use super::*;

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
        // 1.00 + 1.00 = 2.00 USD
        assert!((cost - 2.0).abs() < 1e-9, "expected ~$2.00, got {cost}");
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
        // 1_500_000 micros = $1.50 exactly (representable as f64).
        // Configure pricing so that price_tokens returns exactly 1_500_000.
        // input_cost_per_mtok = 1.5 USD; 1M input tokens with no output yields
        // 1_500_000 micros.
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
}
