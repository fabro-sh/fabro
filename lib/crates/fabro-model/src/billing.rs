use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{Model, Provider};

const TOKENS_PER_MTOK: i128 = 1_000_000;
const ANTHROPIC_FAST_MODE_MULTIPLIER_NUMERATOR: i64 = 6;
const ANTHROPIC_FAST_MODE_MULTIPLIER_DENOMINATOR: i64 = 1;
const ANTHROPIC_CACHE_WRITE_5M_NUMERATOR: i64 = 5;
const ANTHROPIC_CACHE_WRITE_5M_DENOMINATOR: i64 = 4;
const ANTHROPIC_CACHE_WRITE_1H_NUMERATOR: i64 = 2;
const ANTHROPIC_CACHE_WRITE_1H_DENOMINATOR: i64 = 1;
const USD_MICROS_PER_USD_F64: f64 = 1_000_000.0;

fn saturating_i128_to_i64(value: i128) -> i64 {
    i64::try_from(value).unwrap_or_else(|_| {
        if value.is_negative() {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn saturating_rounded_f64_to_i64(value: f64) -> i64 {
    if !value.is_finite() {
        return if value.is_sign_negative() {
            i64::MIN
        } else {
            i64::MAX
        };
    }

    if value <= i64::MIN as f64 {
        i64::MIN
    } else if value >= i64::MAX as f64 {
        i64::MAX
    } else {
        value as i64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub struct UsdMicros(pub i64);

impl std::ops::Add for UsdMicros {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl std::ops::AddAssign for UsdMicros {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl std::iter::Sum for UsdMicros {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |acc, value| acc + value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricePerMTok {
    pub usd_micros: i64,
}

impl PricePerMTok {
    #[must_use]
    pub fn from_usd(usd: f64) -> Self {
        Self {
            usd_micros: saturating_rounded_f64_to_i64((usd * USD_MICROS_PER_USD_F64).round()),
        }
    }

    #[must_use]
    pub fn multiply_ratio(self, numerator: i64, denominator: i64) -> Self {
        Self {
            usd_micros: self.usd_micros.saturating_mul(numerator) / denominator,
        }
    }

    #[must_use]
    pub fn bill(self, tokens: i64) -> UsdMicros {
        let total = i128::from(tokens) * i128::from(self.usd_micros);
        UsdMicros(saturating_i128_to_i64(total / TOKENS_PER_MTOK))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speed {
    Standard,
    Fast,
}

impl Speed {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Fast => "fast",
        }
    }
}

impl FromStr for Speed {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "standard" => Ok(Self::Standard),
            "fast" => Ok(Self::Fast),
            other => Err(format!("unknown speed: {other}")),
        }
    }
}

impl std::fmt::Display for Speed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: Provider,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<Speed>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenCounts {
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(default)]
    pub reasoning_tokens: i64,
    #[serde(default)]
    pub cache_read_tokens: i64,
    #[serde(default)]
    pub cache_write_tokens: i64,
}

impl TokenCounts {
    #[must_use]
    pub fn billable_output_tokens(&self) -> i64 {
        self.output_tokens + self.reasoning_tokens
    }

    #[must_use]
    pub fn total_tokens(&self) -> i64 {
        self.input_tokens
            + self.billable_output_tokens()
            + self.cache_read_tokens
            + self.cache_write_tokens
    }
}

impl std::ops::Add for TokenCounts {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            input_tokens: self.input_tokens + rhs.input_tokens,
            output_tokens: self.output_tokens + rhs.output_tokens,
            reasoning_tokens: self.reasoning_tokens + rhs.reasoning_tokens,
            cache_read_tokens: self.cache_read_tokens + rhs.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens + rhs.cache_write_tokens,
        }
    }
}

impl std::ops::AddAssign for TokenCounts {
    fn add_assign(&mut self, rhs: Self) {
        self.input_tokens += rhs.input_tokens;
        self.output_tokens += rhs.output_tokens;
        self.reasoning_tokens += rhs.reasoning_tokens;
        self.cache_read_tokens += rhs.cache_read_tokens;
        self.cache_write_tokens += rhs.cache_write_tokens;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    pub model: ModelRef,
    pub tokens: TokenCounts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiModelPricing {
    pub input: PricePerMTok,
    pub cached_input: Option<PricePerMTok>,
    pub output: PricePerMTok,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicModelPricing {
    pub input: PricePerMTok,
    pub cache_read: Option<PricePerMTok>,
    pub cache_write_5m: Option<PricePerMTok>,
    pub cache_write_1h: Option<PricePerMTok>,
    pub output: PricePerMTok,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeminiStorageSegment {
    pub cached_tokens: i64,
    pub ttl_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeminiStoragePricing {
    pub usd_micros_per_mtok_second: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeminiModelPricing {
    pub input: PricePerMTok,
    pub output: PricePerMTok,
    pub cached_input: Option<PricePerMTok>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<GeminiStoragePricing>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum ModelPricingPolicy {
    OpenAi(OpenAiModelPricing),
    OpenAiCompatible(OpenAiModelPricing),
    Anthropic(AnthropicModelPricing),
    Gemini(GeminiModelPricing),
    Kimi(OpenAiModelPricing),
    Zai(OpenAiModelPricing),
    Minimax(OpenAiModelPricing),
    Inception(OpenAiModelPricing),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPricing {
    pub model: ModelRef,
    pub policy: ModelPricingPolicy,
}

#[allow(clippy::empty_structs_with_brackets)] // Serialize as `{}` rather than unit-struct `null`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OpenAiBillingFacts {}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AnthropicBillingFacts {
    #[serde(default)]
    pub cache_write_5m_tokens: i64,
    #[serde(default)]
    pub cache_write_1h_tokens: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GeminiBillingFacts {
    #[serde(default)]
    pub storage_segments: Vec<GeminiStorageSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum ModelBillingFacts {
    OpenAi(OpenAiBillingFacts),
    OpenAiCompatible(OpenAiBillingFacts),
    Anthropic(AnthropicBillingFacts),
    Gemini(GeminiBillingFacts),
    Kimi(OpenAiBillingFacts),
    Zai(OpenAiBillingFacts),
    Minimax(OpenAiBillingFacts),
    Inception(OpenAiBillingFacts),
}

impl ModelBillingFacts {
    #[must_use]
    pub fn for_provider(provider: Provider) -> Self {
        match provider {
            Provider::OpenAi => Self::OpenAi(OpenAiBillingFacts::default()),
            Provider::OpenAiCompatible => Self::OpenAiCompatible(OpenAiBillingFacts::default()),
            Provider::Anthropic => Self::Anthropic(AnthropicBillingFacts::default()),
            Provider::Gemini => Self::Gemini(GeminiBillingFacts::default()),
            Provider::Kimi => Self::Kimi(OpenAiBillingFacts::default()),
            Provider::Zai => Self::Zai(OpenAiBillingFacts::default()),
            Provider::Minimax => Self::Minimax(OpenAiBillingFacts::default()),
            Provider::Inception => Self::Inception(OpenAiBillingFacts::default()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelBillingInput {
    pub usage: ModelUsage,
    pub facts: ModelBillingFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BilledModelUsage {
    pub input: ModelBillingInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_usd_micros: Option<i64>,
}

impl BilledModelUsage {
    #[must_use]
    pub fn model(&self) -> &ModelRef {
        &self.input.usage.model
    }

    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.input.usage.model.model_id
    }

    #[must_use]
    pub fn tokens(&self) -> &TokenCounts {
        &self.input.usage.tokens
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BilledTokenCounts {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    #[serde(default)]
    pub reasoning_tokens: i64,
    #[serde(default)]
    pub cache_read_tokens: i64,
    #[serde(default)]
    pub cache_write_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_usd_micros: Option<i64>,
}

impl BilledTokenCounts {
    #[must_use]
    pub fn from_billed_usage(billed: &[BilledModelUsage]) -> Self {
        let mut tokens = TokenCounts::default();
        let mut total_usd_micros = 0_i64;
        let mut has_total = false;

        for entry in billed {
            tokens += entry.input.usage.tokens.clone();
            if let Some(value) = entry.total_usd_micros {
                total_usd_micros += value;
                has_total = true;
            }
        }

        Self {
            input_tokens: tokens.input_tokens,
            output_tokens: tokens.output_tokens,
            total_tokens: tokens.total_tokens(),
            reasoning_tokens: tokens.reasoning_tokens,
            cache_read_tokens: tokens.cache_read_tokens,
            cache_write_tokens: tokens.cache_write_tokens,
            total_usd_micros: has_total.then_some(total_usd_micros),
        }
    }
}

impl Model {
    #[must_use]
    pub fn billing_model_ref(&self, speed: Option<Speed>) -> ModelRef {
        ModelRef {
            provider: self.provider,
            model_id: self.id.clone(),
            speed,
        }
    }

    #[must_use]
    pub fn pricing_for(&self, speed: Option<Speed>) -> Option<ModelPricing> {
        let input = self.costs.input_cost_per_mtok.map(PricePerMTok::from_usd)?;
        let output = self
            .costs
            .output_cost_per_mtok
            .map(PricePerMTok::from_usd)?;
        let cached_input = self
            .costs
            .cache_input_cost_per_mtok
            .map(PricePerMTok::from_usd);

        let (input, output, cached_input) = match (self.provider, speed) {
            (Provider::Anthropic, Some(Speed::Fast)) if self.id == "claude-opus-4-6" => (
                input.multiply_ratio(
                    ANTHROPIC_FAST_MODE_MULTIPLIER_NUMERATOR,
                    ANTHROPIC_FAST_MODE_MULTIPLIER_DENOMINATOR,
                ),
                output.multiply_ratio(
                    ANTHROPIC_FAST_MODE_MULTIPLIER_NUMERATOR,
                    ANTHROPIC_FAST_MODE_MULTIPLIER_DENOMINATOR,
                ),
                cached_input.map(|rate| {
                    rate.multiply_ratio(
                        ANTHROPIC_FAST_MODE_MULTIPLIER_NUMERATOR,
                        ANTHROPIC_FAST_MODE_MULTIPLIER_DENOMINATOR,
                    )
                }),
            ),
            (_, None | Some(Speed::Standard)) => (input, output, cached_input),
            _ => return None,
        };

        let policy = match self.provider {
            Provider::OpenAi => ModelPricingPolicy::OpenAi(OpenAiModelPricing {
                input,
                cached_input,
                output,
            }),
            Provider::OpenAiCompatible => {
                ModelPricingPolicy::OpenAiCompatible(OpenAiModelPricing {
                    input,
                    cached_input,
                    output,
                })
            }
            Provider::Anthropic => ModelPricingPolicy::Anthropic(AnthropicModelPricing {
                input,
                cache_read: cached_input,
                cache_write_5m: Some(input.multiply_ratio(
                    ANTHROPIC_CACHE_WRITE_5M_NUMERATOR,
                    ANTHROPIC_CACHE_WRITE_5M_DENOMINATOR,
                )),
                cache_write_1h: Some(input.multiply_ratio(
                    ANTHROPIC_CACHE_WRITE_1H_NUMERATOR,
                    ANTHROPIC_CACHE_WRITE_1H_DENOMINATOR,
                )),
                output,
            }),
            Provider::Gemini => ModelPricingPolicy::Gemini(GeminiModelPricing {
                input,
                output,
                cached_input,
                storage: None,
            }),
            Provider::Kimi => ModelPricingPolicy::Kimi(OpenAiModelPricing {
                input,
                cached_input,
                output,
            }),
            Provider::Zai => ModelPricingPolicy::Zai(OpenAiModelPricing {
                input,
                cached_input,
                output,
            }),
            Provider::Minimax => ModelPricingPolicy::Minimax(OpenAiModelPricing {
                input,
                cached_input,
                output,
            }),
            Provider::Inception => ModelPricingPolicy::Inception(OpenAiModelPricing {
                input,
                cached_input,
                output,
            }),
        };

        Some(ModelPricing {
            model: self.billing_model_ref(speed),
            policy,
        })
    }
}

impl ModelPricing {
    #[must_use]
    pub fn bill(&self, input: &ModelBillingInput) -> Option<UsdMicros> {
        if input.usage.model != self.model {
            return None;
        }

        let bill = match (&self.policy, &input.facts) {
            (ModelPricingPolicy::OpenAi(pricing), ModelBillingFacts::OpenAi(_))
            | (
                ModelPricingPolicy::OpenAiCompatible(pricing),
                ModelBillingFacts::OpenAiCompatible(_),
            )
            | (ModelPricingPolicy::Kimi(pricing), ModelBillingFacts::Kimi(_))
            | (ModelPricingPolicy::Zai(pricing), ModelBillingFacts::Zai(_))
            | (ModelPricingPolicy::Minimax(pricing), ModelBillingFacts::Minimax(_))
            | (ModelPricingPolicy::Inception(pricing), ModelBillingFacts::Inception(_)) => {
                Some(bill_openai_like(pricing, &input.usage.tokens))
            }
            (ModelPricingPolicy::Anthropic(pricing), ModelBillingFacts::Anthropic(facts)) => {
                Some(bill_anthropic(pricing, &input.usage.tokens, facts))
            }
            (ModelPricingPolicy::Gemini(pricing), ModelBillingFacts::Gemini(facts)) => {
                bill_gemini(pricing, &input.usage.tokens, facts)
            }
            _ => None,
        }?;

        Some(bill)
    }

    #[must_use]
    pub fn bill_usage(&self, input: ModelBillingInput) -> BilledModelUsage {
        let total_usd_micros = self.bill(&input).map(|amount| amount.0);
        BilledModelUsage {
            input,
            total_usd_micros,
        }
    }
}

fn bill_openai_like(pricing: &OpenAiModelPricing, tokens: &TokenCounts) -> UsdMicros {
    let mut total = pricing.input.bill(tokens.input_tokens);
    total += pricing.output.bill(tokens.billable_output_tokens());
    if let Some(cached_input) = pricing.cached_input {
        total += cached_input.bill(tokens.cache_read_tokens);
    }
    total
}

fn bill_anthropic(
    pricing: &AnthropicModelPricing,
    tokens: &TokenCounts,
    facts: &AnthropicBillingFacts,
) -> UsdMicros {
    let mut total = pricing.input.bill(tokens.input_tokens);
    total += pricing.output.bill(tokens.billable_output_tokens());
    if let Some(cache_read) = pricing.cache_read {
        total += cache_read.bill(tokens.cache_read_tokens);
    }
    if let Some(cache_write_5m) = pricing.cache_write_5m {
        total += cache_write_5m.bill(facts.cache_write_5m_tokens);
    }
    if let Some(cache_write_1h) = pricing.cache_write_1h {
        total += cache_write_1h.bill(facts.cache_write_1h_tokens);
    }
    total
}

fn bill_gemini(
    pricing: &GeminiModelPricing,
    tokens: &TokenCounts,
    facts: &GeminiBillingFacts,
) -> Option<UsdMicros> {
    if tokens.cache_read_tokens > 0 && pricing.cached_input.is_none() {
        return None;
    }
    if !facts.storage_segments.is_empty() && pricing.storage.is_none() {
        return None;
    }

    let mut total = pricing.input.bill(tokens.input_tokens);
    total += pricing.output.bill(tokens.billable_output_tokens());
    if let Some(cached_input) = pricing.cached_input {
        total += cached_input.bill(tokens.cache_read_tokens);
    }
    if let Some(storage) = pricing.storage.as_ref() {
        let storage_cost = facts
            .storage_segments
            .iter()
            .map(|segment| {
                let token_seconds =
                    i128::from(segment.cached_tokens) * i128::from(segment.ttl_seconds);
                UsdMicros(saturating_i128_to_i64(
                    token_seconds * i128::from(storage.usd_micros_per_mtok_second)
                        / TOKENS_PER_MTOK,
                ))
            })
            .sum::<UsdMicros>();
        total += storage_cost;
    }

    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Catalog;

    #[test]
    fn openai_pricing_bills_cached_input_and_reasoning_output() {
        let pricing = ModelPricing {
            model: ModelRef {
                provider: Provider::OpenAi,
                model_id: "gpt-5.4".to_string(),
                speed: None,
            },
            policy: ModelPricingPolicy::OpenAi(OpenAiModelPricing {
                input: PricePerMTok {
                    usd_micros: 1_250_000,
                },
                cached_input: Some(PricePerMTok {
                    usd_micros: 125_000,
                }),
                output: PricePerMTok {
                    usd_micros: 10_000_000,
                },
            }),
        };
        let input = ModelBillingInput {
            usage: ModelUsage {
                model: pricing.model.clone(),
                tokens: TokenCounts {
                    input_tokens: 500_000,
                    output_tokens: 125_000,
                    reasoning_tokens: 25_000,
                    cache_read_tokens: 250_000,
                    cache_write_tokens: 0,
                },
            },
            facts: ModelBillingFacts::OpenAi(OpenAiBillingFacts::default()),
        };

        assert_eq!(pricing.bill(&input), Some(UsdMicros(2_156_250)));
    }

    #[test]
    fn anthropic_fast_mode_derives_cache_write_rates_from_base_input() {
        let model = Catalog::builtin().get("claude-opus-4-6").unwrap();
        let pricing = model.pricing_for(Some(Speed::Fast)).unwrap();

        let ModelPricingPolicy::Anthropic(anthropic) = pricing.policy else {
            panic!("expected anthropic pricing");
        };

        assert_eq!(anthropic.input.usd_micros, 30_000_000);
        assert_eq!(anthropic.output.usd_micros, 150_000_000);
        assert_eq!(anthropic.cache_read.unwrap().usd_micros, 3_000_000);
        assert_eq!(anthropic.cache_write_5m.unwrap().usd_micros, 37_500_000);
        assert_eq!(anthropic.cache_write_1h.unwrap().usd_micros, 60_000_000);
    }

    #[test]
    fn anthropic_billing_supports_distinct_cache_write_buckets() {
        let pricing = ModelPricing {
            model: ModelRef {
                provider: Provider::Anthropic,
                model_id: "claude-opus-4-6".to_string(),
                speed: Some(Speed::Fast),
            },
            policy: ModelPricingPolicy::Anthropic(AnthropicModelPricing {
                input: PricePerMTok {
                    usd_micros: 30_000_000,
                },
                cache_read: Some(PricePerMTok {
                    usd_micros: 3_000_000,
                }),
                cache_write_5m: Some(PricePerMTok {
                    usd_micros: 37_500_000,
                }),
                cache_write_1h: Some(PricePerMTok {
                    usd_micros: 60_000_000,
                }),
                output: PricePerMTok {
                    usd_micros: 150_000_000,
                },
            }),
        };
        let input = ModelBillingInput {
            usage: ModelUsage {
                model: pricing.model.clone(),
                tokens: TokenCounts {
                    input_tokens: 100_000,
                    output_tokens: 10_000,
                    reasoning_tokens: 5_000,
                    cache_read_tokens: 20_000,
                    cache_write_tokens: 0,
                },
            },
            facts: ModelBillingFacts::Anthropic(AnthropicBillingFacts {
                cache_write_5m_tokens: 30_000,
                cache_write_1h_tokens: 40_000,
            }),
        };

        assert_eq!(pricing.bill(&input), Some(UsdMicros(8_835_000)));
    }

    #[test]
    fn gemini_billing_requires_storage_pricing_when_storage_facts_exist() {
        let pricing = ModelPricing {
            model: ModelRef {
                provider: Provider::Gemini,
                model_id: "gemini-3.1-pro-preview".to_string(),
                speed: None,
            },
            policy: ModelPricingPolicy::Gemini(GeminiModelPricing {
                input: PricePerMTok {
                    usd_micros: 1_250_000,
                },
                output: PricePerMTok {
                    usd_micros: 10_000_000,
                },
                cached_input: None,
                storage: None,
            }),
        };
        let input = ModelBillingInput {
            usage: ModelUsage {
                model: pricing.model.clone(),
                tokens: TokenCounts {
                    input_tokens: 100_000,
                    output_tokens: 10_000,
                    reasoning_tokens: 0,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                },
            },
            facts: ModelBillingFacts::Gemini(GeminiBillingFacts {
                storage_segments: vec![GeminiStorageSegment {
                    cached_tokens: 100_000,
                    ttl_seconds: 60,
                }],
            }),
        };

        assert_eq!(pricing.bill(&input), None);
    }

    #[test]
    fn price_per_mtok_bill_saturates_large_totals() {
        let price = PricePerMTok {
            usd_micros: i64::MAX,
        };

        assert_eq!(price.bill(i64::MAX), UsdMicros(i64::MAX));
    }

    #[test]
    fn price_per_mtok_from_usd_saturates_large_inputs() {
        let price = PricePerMTok::from_usd(f64::MAX);

        assert_eq!(price.usd_micros, i64::MAX);
    }

    #[test]
    fn openai_billing_facts_serialize_as_empty_object() {
        assert_eq!(
            serde_json::to_value(OpenAiBillingFacts::default()).unwrap(),
            serde_json::json!({})
        );
    }
}
