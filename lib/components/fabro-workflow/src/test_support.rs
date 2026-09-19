use fabro_types::ModelRef;
use lithos_llm::catalog::{ModelId, builtin};
use lithos_llm::types::{Cost, CostSource, TokenCounts, Usage};

/// Construct a fully-populated `ModelUsage` for tests: `input_tokens` and
/// `output_tokens` on an OpenAI model, priced from the catalog at one micro
/// per token. Centralised so callers don't keep rebuilding the same skeleton.
#[must_use]
pub fn test_usage(
    model_id: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> fabro_types::ModelUsage {
    fabro_types::ModelUsage::new(
        ModelRef::new(builtin::openai(), ModelId::new(model_id)),
        Usage {
            tokens: TokenCounts {
                input: input_tokens,
                output: output_tokens,
                ..TokenCounts::default()
            },
            cost:   Some(Cost {
                usd_micros: input_tokens.saturating_add(output_tokens),
                source:     CostSource::Catalog,
            }),
        },
    )
}
