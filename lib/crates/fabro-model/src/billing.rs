use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

use crate::catalog::{BillingPolicy, Catalog, CatalogModelSettings};
use crate::{Model, ModelCosts, ProviderId};

const TOKENS_PER_MTOK: i128 = 1_000_000;
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

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "Billing rounds bounded finite floats into i64 counters by design."
)]
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

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
    strum::VariantArray,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Speed {
    Standard,
    Fast,
}

impl Speed {
    #[must_use]
    pub fn variants() -> &'static [Self] {
        <Self as strum::VariantArray>::VARIANTS
    }
}

/// Source of a USD cost value attached to a completion response.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum CostSource {
    /// The provider returned billing data in-band with the response.
    Authoritative,
    /// Computed from catalog prices and token usage.
    Estimated,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: ProviderId,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed:    Option<Speed>,
}

/// Token counts for one LLM call.
///
/// All five fields are disjoint: each token is counted in exactly one bucket,
/// and `total_tokens()` is their sum. Provider mappings normalize their wire
/// formats into this shape. For example, OpenAI's nested cached tokens are
/// subtracted out of `input_tokens`, while Anthropic thinking tokens remain in
/// `output_tokens` because Anthropic does not expose a separate billed count.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenCounts {
    pub input_tokens:       i64,
    pub output_tokens:      i64,
    #[serde(default)]
    pub reasoning_tokens:   i64,
    #[serde(default)]
    pub cache_read_tokens:  i64,
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
            input_tokens:       self.input_tokens + rhs.input_tokens,
            output_tokens:      self.output_tokens + rhs.output_tokens,
            reasoning_tokens:   self.reasoning_tokens + rhs.reasoning_tokens,
            cache_read_tokens:  self.cache_read_tokens + rhs.cache_read_tokens,
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
    pub model:  ModelRef,
    pub tokens: TokenCounts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiModelPricing {
    pub input:        PricePerMTok,
    pub cached_input: Option<PricePerMTok>,
    pub output:       PricePerMTok,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicModelPricing {
    pub input:          PricePerMTok,
    pub cache_read:     Option<PricePerMTok>,
    pub cache_write_5m: Option<PricePerMTok>,
    pub cache_write_1h: Option<PricePerMTok>,
    pub output:         PricePerMTok,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeminiStorageSegment {
    pub cached_tokens: i64,
    pub ttl_seconds:   i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeminiStoragePricing {
    pub usd_micros_per_mtok_second: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeminiModelPricing {
    pub input:        PricePerMTok,
    pub output:       PricePerMTok,
    pub cached_input: Option<PricePerMTok>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage:      Option<GeminiStoragePricing>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "algorithm", rename_all = "snake_case")]
pub enum ModelPricingPolicy {
    #[serde(rename = "openai")]
    OpenAi(OpenAiModelPricing),
    Anthropic(AnthropicModelPricing),
    Gemini(GeminiModelPricing),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPricing {
    pub model:  ModelRef,
    pub policy: ModelPricingPolicy,
}

#[allow(
    clippy::empty_structs_with_brackets,
    reason = "This type must serialize as {} rather than null."
)]
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
#[serde(tag = "algorithm", rename_all = "snake_case")]
pub enum ModelBillingFacts {
    #[serde(rename = "openai")]
    OpenAi(OpenAiBillingFacts),
    Anthropic(AnthropicBillingFacts),
    Gemini(GeminiBillingFacts),
}

impl ModelBillingFacts {
    #[must_use]
    pub fn for_policy(policy: BillingPolicy, tokens: &TokenCounts) -> Option<Self> {
        match policy {
            BillingPolicy::OpenAi => Some(Self::OpenAi(OpenAiBillingFacts::default())),
            BillingPolicy::Anthropic => Some(Self::Anthropic(anthropic_billing_facts(tokens))),
            BillingPolicy::Gemini => Some(Self::Gemini(GeminiBillingFacts::default())),
            BillingPolicy::None => None,
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
    pub input:            ModelBillingInput,
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
    pub input_tokens:       i64,
    pub output_tokens:      i64,
    pub total_tokens:       i64,
    #[serde(default)]
    pub reasoning_tokens:   i64,
    #[serde(default)]
    pub cache_read_tokens:  i64,
    #[serde(default)]
    pub cache_write_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_usd_micros:   Option<i64>,
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
            input_tokens:       tokens.input_tokens,
            output_tokens:      tokens.output_tokens,
            total_tokens:       tokens.total_tokens(),
            reasoning_tokens:   tokens.reasoning_tokens,
            cache_read_tokens:  tokens.cache_read_tokens,
            cache_write_tokens: tokens.cache_write_tokens,
            total_usd_micros:   has_total.then_some(total_usd_micros),
        }
    }

    /// Returns the five disjoint per-call token buckets, dropping the derived
    /// `total_tokens` sum and the optional `total_usd_micros` cost.
    #[must_use]
    pub fn token_counts(&self) -> TokenCounts {
        TokenCounts {
            input_tokens:       self.input_tokens,
            output_tokens:      self.output_tokens,
            reasoning_tokens:   self.reasoning_tokens,
            cache_read_tokens:  self.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens,
        }
    }

    pub fn add_counts(&mut self, source: &Self) {
        self.input_tokens += source.input_tokens;
        self.output_tokens += source.output_tokens;
        self.total_tokens += source.total_tokens;
        self.reasoning_tokens += source.reasoning_tokens;
        self.cache_read_tokens += source.cache_read_tokens;
        self.cache_write_tokens += source.cache_write_tokens;
        if let Some(value) = source.total_usd_micros {
            *self.total_usd_micros.get_or_insert(0) += value;
        }
    }

    pub fn add_billed_usage(&mut self, usage: &BilledModelUsage) {
        let tokens = usage.tokens();
        self.input_tokens += tokens.input_tokens;
        self.output_tokens += tokens.output_tokens;
        self.reasoning_tokens += tokens.reasoning_tokens;
        self.cache_read_tokens += tokens.cache_read_tokens;
        self.cache_write_tokens += tokens.cache_write_tokens;
        self.total_tokens += tokens.total_tokens();
        if let Some(value) = usage.total_usd_micros {
            *self.total_usd_micros.get_or_insert(0) += value;
        }
    }

    pub fn replace_with_billed_usage(&mut self, usage: &BilledModelUsage) {
        *self = Self::from_billed_usage(std::slice::from_ref(usage));
    }

    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.total_tokens == 0
            && self.reasoning_tokens == 0
            && self.cache_read_tokens == 0
            && self.cache_write_tokens == 0
            && self.total_usd_micros.unwrap_or(0) == 0
    }
}

fn anthropic_billing_facts(tokens: &TokenCounts) -> AnthropicBillingFacts {
    AnthropicBillingFacts {
        cache_write_5m_tokens: tokens.cache_write_tokens,
        cache_write_1h_tokens: 0,
    }
}

impl Catalog {
    #[must_use]
    pub fn pricing_for(&self, model_ref: &ModelRef) -> Option<ModelPricing> {
        let model = self.get(&model_ref.model_id)?;
        let provider = self.provider(&model_ref.provider)?;
        if model.provider != provider.id {
            return None;
        }

        let settings = self.model_settings(&model.id)?;
        let costs = costs_for_speed(model, settings, model_ref.speed)?;
        pricing_for_model_costs(
            model,
            provider.id.clone(),
            provider.billing_policy,
            model_ref.speed,
            &costs,
        )
    }

    #[must_use]
    pub fn billing_facts_for(
        &self,
        model_ref: &ModelRef,
        tokens: &TokenCounts,
    ) -> Option<ModelBillingFacts> {
        self.provider(&model_ref.provider)
            .and_then(|provider| ModelBillingFacts::for_policy(provider.billing_policy, tokens))
    }

    /// Price a partial token sample for `model` using catalog pricing.
    ///
    /// Returns `None` when the provider has no billing policy, the model is
    /// unknown, or the pricing algorithm cannot produce a result for the given
    /// tokens. Used by read-side rollups so in-flight stages can show an
    /// exact cost for the tokens consumed so far.
    #[must_use]
    pub fn price_tokens(&self, model: &ModelRef, tokens: &TokenCounts) -> Option<i64> {
        let facts = self.billing_facts_for(model, tokens)?;
        let input = ModelBillingInput {
            usage: ModelUsage {
                model:  model.clone(),
                tokens: tokens.clone(),
            },
            facts,
        };
        self.pricing_for(model)
            .and_then(|pricing| pricing.bill(&input))
            .map(|amount| amount.0)
    }
}

fn costs_for_speed(
    model: &Model,
    settings: &CatalogModelSettings,
    speed: Option<Speed>,
) -> Option<ModelCosts> {
    match speed {
        None | Some(Speed::Standard) => Some(model.costs.clone()),
        Some(speed) => {
            if !settings.controls.speed.contains(&speed) {
                return None;
            }
            let Some(speed_costs) = settings.speed_costs.get(&speed) else {
                return Some(model.costs.clone());
            };
            Some(merge_cost_override(&model.costs, speed_costs))
        }
    }
}

fn merge_cost_override(base: &ModelCosts, override_costs: &ModelCosts) -> ModelCosts {
    ModelCosts {
        input_cost_per_mtok:       override_costs
            .input_cost_per_mtok
            .or(base.input_cost_per_mtok),
        output_cost_per_mtok:      override_costs
            .output_cost_per_mtok
            .or(base.output_cost_per_mtok),
        cache_input_cost_per_mtok: override_costs
            .cache_input_cost_per_mtok
            .or(base.cache_input_cost_per_mtok),
    }
}

impl Model {
    #[must_use]
    pub fn billing_model_ref(&self, speed: Option<Speed>) -> ModelRef {
        ModelRef {
            provider: self.provider.clone(),
            model_id: self.id.clone(),
            speed,
        }
    }
}

fn pricing_for_model_costs(
    model: &Model,
    provider_id: ProviderId,
    billing_policy: BillingPolicy,
    speed: Option<Speed>,
    costs: &ModelCosts,
) -> Option<ModelPricing> {
    let input = costs.input_cost_per_mtok.map(PricePerMTok::from_usd)?;
    let output = costs.output_cost_per_mtok.map(PricePerMTok::from_usd)?;
    let cached_input = costs.cache_input_cost_per_mtok.map(PricePerMTok::from_usd);

    let policy = pricing_policy_for_billing_policy(billing_policy, input, output, cached_input)?;
    Some(ModelPricing {
        model: ModelRef {
            provider: provider_id,
            model_id: model.id.clone(),
            speed,
        },
        policy,
    })
}

fn pricing_policy_for_billing_policy(
    billing_policy: BillingPolicy,
    input: PricePerMTok,
    output: PricePerMTok,
    cached_input: Option<PricePerMTok>,
) -> Option<ModelPricingPolicy> {
    match billing_policy {
        BillingPolicy::Anthropic => Some(anthropic_pricing_policy(input, output, cached_input)),
        BillingPolicy::Gemini => Some(ModelPricingPolicy::Gemini(GeminiModelPricing {
            input,
            output,
            cached_input,
            storage: None,
        })),
        BillingPolicy::OpenAi => Some(ModelPricingPolicy::OpenAi(OpenAiModelPricing {
            input,
            cached_input,
            output,
        })),
        BillingPolicy::None => None,
    }
}

fn anthropic_pricing_policy(
    input: PricePerMTok,
    output: PricePerMTok,
    cached_input: Option<PricePerMTok>,
) -> ModelPricingPolicy {
    ModelPricingPolicy::Anthropic(AnthropicModelPricing {
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
    })
}

impl ModelPricing {
    #[must_use]
    pub fn bill(&self, input: &ModelBillingInput) -> Option<UsdMicros> {
        if input.usage.model != self.model {
            return None;
        }

        let bill = match (&self.policy, &input.facts) {
            (ModelPricingPolicy::OpenAi(pricing), ModelBillingFacts::OpenAi(_)) => {
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
    use crate::catalog::LlmCatalogSettings;
    use crate::{Catalog, ProviderId};

    fn catalog_from_toml(source: &str) -> Catalog {
        let settings: LlmCatalogSettings =
            toml::from_str(source).expect("catalog fixture should parse");
        Catalog::from_settings(&settings).expect("catalog fixture should build")
    }

    fn billed_usage(
        input_tokens: i64,
        output_tokens: i64,
        total_usd_micros: Option<i64>,
    ) -> BilledModelUsage {
        BilledModelUsage {
            input: ModelBillingInput {
                usage: ModelUsage {
                    model:  ModelRef {
                        provider: ProviderId::openai(),
                        model_id: "gpt-5.4".to_string(),
                        speed:    None,
                    },
                    tokens: TokenCounts {
                        input_tokens,
                        output_tokens,
                        reasoning_tokens: 3,
                        cache_read_tokens: 5,
                        cache_write_tokens: 7,
                    },
                },
                facts: ModelBillingFacts::OpenAi(OpenAiBillingFacts::default()),
            },
            total_usd_micros,
        }
    }

    #[test]
    fn billed_token_counts_add_counts_accumulates_cost_when_known() {
        let mut counts = BilledTokenCounts {
            input_tokens:       1,
            output_tokens:      2,
            total_tokens:       3,
            reasoning_tokens:   4,
            cache_read_tokens:  5,
            cache_write_tokens: 6,
            total_usd_micros:   None,
        };
        counts.add_counts(&BilledTokenCounts {
            input_tokens:       10,
            output_tokens:      20,
            total_tokens:       30,
            reasoning_tokens:   40,
            cache_read_tokens:  50,
            cache_write_tokens: 60,
            total_usd_micros:   Some(70),
        });

        assert_eq!(counts, BilledTokenCounts {
            input_tokens:       11,
            output_tokens:      22,
            total_tokens:       33,
            reasoning_tokens:   44,
            cache_read_tokens:  55,
            cache_write_tokens: 66,
            total_usd_micros:   Some(70),
        });
    }

    #[test]
    fn billed_token_counts_add_billed_usage_preserves_unknown_cost() {
        let mut counts = BilledTokenCounts::default();

        counts.add_billed_usage(&billed_usage(10, 20, None));

        assert_eq!(counts, BilledTokenCounts {
            input_tokens:       10,
            output_tokens:      20,
            total_tokens:       45,
            reasoning_tokens:   3,
            cache_read_tokens:  5,
            cache_write_tokens: 7,
            total_usd_micros:   None,
        });
    }

    #[test]
    fn billed_token_counts_add_billed_usage_accumulates_known_cost() {
        let mut counts = BilledTokenCounts::default();

        counts.add_billed_usage(&billed_usage(10, 20, Some(100)));
        counts.add_billed_usage(&billed_usage(1, 2, Some(50)));

        assert_eq!(counts.input_tokens, 11);
        assert_eq!(counts.output_tokens, 22);
        assert_eq!(counts.total_tokens, 63);
        assert_eq!(counts.total_usd_micros, Some(150));
    }

    #[test]
    fn billed_token_counts_replace_with_billed_usage_discards_previous_values() {
        let mut counts = BilledTokenCounts {
            input_tokens:       100,
            output_tokens:      200,
            total_tokens:       300,
            reasoning_tokens:   400,
            cache_read_tokens:  500,
            cache_write_tokens: 600,
            total_usd_micros:   Some(700),
        };

        counts.replace_with_billed_usage(&billed_usage(1, 2, None));

        assert_eq!(counts, BilledTokenCounts {
            input_tokens:       1,
            output_tokens:      2,
            total_tokens:       18,
            reasoning_tokens:   3,
            cache_read_tokens:  5,
            cache_write_tokens: 7,
            total_usd_micros:   None,
        });
    }

    #[test]
    fn billed_token_counts_is_zero_treats_missing_and_zero_cost_as_zero() {
        assert!(BilledTokenCounts::default().is_zero());
        assert!(
            BilledTokenCounts {
                total_usd_micros: Some(0),
                ..BilledTokenCounts::default()
            }
            .is_zero()
        );
        assert!(
            !BilledTokenCounts {
                input_tokens: 1,
                ..BilledTokenCounts::default()
            }
            .is_zero()
        );
        assert!(
            !BilledTokenCounts {
                total_usd_micros: Some(1),
                ..BilledTokenCounts::default()
            }
            .is_zero()
        );
    }

    #[test]
    fn openai_pricing_bills_cached_input_and_reasoning_output() {
        let pricing = ModelPricing {
            model:  ModelRef {
                provider: ProviderId::openai(),
                model_id: "gpt-5.4".to_string(),
                speed:    None,
            },
            policy: ModelPricingPolicy::OpenAi(OpenAiModelPricing {
                input:        PricePerMTok {
                    usd_micros: 1_250_000,
                },
                cached_input: Some(PricePerMTok {
                    usd_micros: 125_000,
                }),
                output:       PricePerMTok {
                    usd_micros: 10_000_000,
                },
            }),
        };
        let input = ModelBillingInput {
            usage: ModelUsage {
                model:  pricing.model.clone(),
                tokens: TokenCounts {
                    input_tokens:       500_000,
                    output_tokens:      125_000,
                    reasoning_tokens:   25_000,
                    cache_read_tokens:  250_000,
                    cache_write_tokens: 0,
                },
            },
            facts: ModelBillingFacts::OpenAi(OpenAiBillingFacts::default()),
        };

        assert_eq!(pricing.bill(&input), Some(UsdMicros(2_156_250)));
    }

    #[test]
    fn catalog_pricing_uses_speed_cost_overrides() {
        let pricing = Catalog::builtin()
            .pricing_for(&ModelRef {
                provider: ProviderId::anthropic(),
                model_id: "claude-opus-4-6".to_string(),
                speed:    Some(Speed::Fast),
            })
            .unwrap();

        let ModelPricingPolicy::Anthropic(anthropic) = pricing.policy else {
            panic!("expected anthropic pricing");
        };

        assert_eq!(pricing.model.provider, ProviderId::anthropic());
        assert_eq!(pricing.model.model_id, "claude-opus-4-6");
        assert_eq!(pricing.model.speed, Some(Speed::Fast));
        assert_eq!(anthropic.input.usd_micros, 30_000_000);
        assert_eq!(anthropic.output.usd_micros, 150_000_000);
        assert_eq!(anthropic.cache_read.unwrap().usd_micros, 3_000_000);
        assert_eq!(anthropic.cache_write_5m.unwrap().usd_micros, 37_500_000);
        assert_eq!(anthropic.cache_write_1h.unwrap().usd_micros, 60_000_000);
    }

    #[test]
    fn catalog_pricing_standard_speed_uses_base_costs() {
        let pricing = Catalog::builtin()
            .pricing_for(&ModelRef {
                provider: ProviderId::anthropic(),
                model_id: "claude-opus-4-6".to_string(),
                speed:    Some(Speed::Standard),
            })
            .unwrap();

        let ModelPricingPolicy::Anthropic(anthropic) = pricing.policy else {
            panic!("expected anthropic pricing");
        };

        assert_eq!(anthropic.input.usd_micros, 5_000_000);
        assert_eq!(anthropic.output.usd_micros, 25_000_000);
        assert_eq!(anthropic.cache_read.unwrap().usd_micros, 500_000);
        assert_eq!(anthropic.cache_write_5m.unwrap().usd_micros, 6_250_000);
        assert_eq!(anthropic.cache_write_1h.unwrap().usd_micros, 10_000_000);
    }

    #[test]
    fn catalog_pricing_supported_fast_without_override_uses_base_costs() {
        let catalog = catalog_from_toml(
            r#"
[providers.test_anthropic]
display_name = "Test Anthropic"
adapter = "anthropic"
agent_profile = "anthropic"
billing_policy = "anthropic"

[models.test-opus]
provider = "test_anthropic"
display_name = "Test Opus"
family = "test"
default = true

[models.test-opus.limits]
context_window = 1000

[models.test-opus.features]
tools = true
vision = false
reasoning = false

[models.test-opus.controls]
speed = ["fast"]

[models.test-opus.costs]
input_cost_per_mtok = 1.0
output_cost_per_mtok = 4.0
cache_input_cost_per_mtok = 0.25
"#,
        );

        let pricing = catalog
            .pricing_for(&ModelRef {
                provider: ProviderId::new("test_anthropic"),
                model_id: "test-opus".to_string(),
                speed:    Some(Speed::Fast),
            })
            .unwrap();

        let ModelPricingPolicy::Anthropic(anthropic) = pricing.policy else {
            panic!("expected anthropic adapter pricing");
        };
        assert_eq!(anthropic.input.usd_micros, 1_000_000);
        assert_eq!(anthropic.output.usd_micros, 4_000_000);
        assert_eq!(anthropic.cache_read.unwrap().usd_micros, 250_000);
    }

    #[test]
    fn catalog_pricing_supports_custom_openai_compatible_provider_costs() {
        let catalog = catalog_from_toml(
            r#"
[providers.proxy]
display_name = "Proxy"
adapter = "openai_compatible"
agent_profile = "openai"
billing_policy = "openai"
base_url = "https://proxy.example/v1"

[models.proxy-model]
provider = "proxy"
display_name = "Proxy Model"
family = "proxy"
default = true

[models.proxy-model.limits]
context_window = 1000

[models.proxy-model.features]
tools = true
vision = false
reasoning = false

[models.proxy-model.costs]
input_cost_per_mtok = 1.0
output_cost_per_mtok = 2.0
cache_input_cost_per_mtok = 0.1
"#,
        );

        let pricing = catalog
            .pricing_for(&ModelRef {
                provider: ProviderId::new("proxy"),
                model_id: "proxy-model".to_string(),
                speed:    None,
            })
            .unwrap();

        let ModelPricingPolicy::OpenAi(openai_like) = pricing.policy else {
            panic!("expected OpenAI billing algorithm for OpenAI-compatible adapter");
        };
        assert_eq!(pricing.model.provider, ProviderId::new("proxy"));
        assert_eq!(openai_like.input.usd_micros, 1_000_000);
        assert_eq!(openai_like.output.usd_micros, 2_000_000);
        assert_eq!(openai_like.cached_input.unwrap().usd_micros, 100_000);
    }

    #[test]
    fn catalog_pricing_uses_canonical_model_id_not_api_id() {
        let catalog = catalog_from_toml(
            r#"
[providers.proxy]
display_name = "Proxy"
adapter = "openai_compatible"
agent_profile = "openai"
billing_policy = "openai"
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
        );

        assert!(
            catalog
                .pricing_for(&ModelRef {
                    provider: ProviderId::new("proxy"),
                    model_id: "canonical-model".to_string(),
                    speed:    None,
                })
                .is_some()
        );
        assert!(
            catalog
                .pricing_for(&ModelRef {
                    provider: ProviderId::new("proxy"),
                    model_id: "wire-model".to_string(),
                    speed:    None,
                })
                .is_none()
        );
    }

    #[test]
    fn catalog_pricing_unknown_provider_model_or_speed_has_no_estimate() {
        assert!(
            Catalog::builtin()
                .pricing_for(&ModelRef {
                    provider: ProviderId::new("unknown"),
                    model_id: "claude-opus-4-6".to_string(),
                    speed:    None,
                })
                .is_none()
        );
        assert!(
            Catalog::builtin()
                .pricing_for(&ModelRef {
                    provider: ProviderId::anthropic(),
                    model_id: "unknown".to_string(),
                    speed:    None,
                })
                .is_none()
        );
        assert!(
            Catalog::builtin()
                .pricing_for(&ModelRef {
                    provider: ProviderId::openai(),
                    model_id: "gpt-5.4".to_string(),
                    speed:    Some(Speed::Fast),
                })
                .is_none()
        );
    }

    #[test]
    fn anthropic_billing_supports_distinct_cache_write_buckets() {
        let pricing = ModelPricing {
            model:  ModelRef {
                provider: ProviderId::anthropic(),
                model_id: "claude-opus-4-6".to_string(),
                speed:    Some(Speed::Fast),
            },
            policy: ModelPricingPolicy::Anthropic(AnthropicModelPricing {
                input:          PricePerMTok {
                    usd_micros: 30_000_000,
                },
                cache_read:     Some(PricePerMTok {
                    usd_micros: 3_000_000,
                }),
                cache_write_5m: Some(PricePerMTok {
                    usd_micros: 37_500_000,
                }),
                cache_write_1h: Some(PricePerMTok {
                    usd_micros: 60_000_000,
                }),
                output:         PricePerMTok {
                    usd_micros: 150_000_000,
                },
            }),
        };
        let input = ModelBillingInput {
            usage: ModelUsage {
                model:  pricing.model.clone(),
                tokens: TokenCounts {
                    input_tokens:       100_000,
                    output_tokens:      10_000,
                    reasoning_tokens:   5_000,
                    cache_read_tokens:  20_000,
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
            model:  ModelRef {
                provider: ProviderId::gemini(),
                model_id: "gemini-3.1-pro-preview".to_string(),
                speed:    None,
            },
            policy: ModelPricingPolicy::Gemini(GeminiModelPricing {
                input:        PricePerMTok {
                    usd_micros: 1_250_000,
                },
                output:       PricePerMTok {
                    usd_micros: 10_000_000,
                },
                cached_input: None,
                storage:      None,
            }),
        };
        let input = ModelBillingInput {
            usage: ModelUsage {
                model:  pricing.model.clone(),
                tokens: TokenCounts {
                    input_tokens:       100_000,
                    output_tokens:      10_000,
                    reasoning_tokens:   0,
                    cache_read_tokens:  0,
                    cache_write_tokens: 0,
                },
            },
            facts: ModelBillingFacts::Gemini(GeminiBillingFacts {
                storage_segments: vec![GeminiStorageSegment {
                    cached_tokens: 100_000,
                    ttl_seconds:   60,
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

    #[test]
    fn pricing_policy_serializes_with_algorithm_tag() {
        let policy = ModelPricingPolicy::OpenAi(OpenAiModelPricing {
            input:        PricePerMTok { usd_micros: 1 },
            cached_input: None,
            output:       PricePerMTok { usd_micros: 2 },
        });

        assert_eq!(
            serde_json::to_value(policy).unwrap(),
            serde_json::json!({
                "algorithm": "openai",
                "input": { "usd_micros": 1 },
                "cached_input": null,
                "output": { "usd_micros": 2 }
            })
        );
    }

    #[test]
    fn old_provider_tagged_billing_facts_are_rejected() {
        let error = serde_json::from_value::<ModelBillingFacts>(serde_json::json!({
            "provider": "openai"
        }))
        .unwrap_err();
        assert!(error.to_string().contains("algorithm"));
    }

    #[test]
    fn old_provider_tagged_pricing_policy_is_rejected() {
        let error =
            serde_json::from_value::<ModelPricingPolicy>(openai_pricing_json("provider", "kimi"))
                .unwrap_err();
        assert!(error.to_string().contains("algorithm"));
    }

    #[test]
    fn openai_billing_policy_uses_openai_billing_algorithm() {
        let facts =
            ModelBillingFacts::for_policy(BillingPolicy::OpenAi, &TokenCounts::default()).unwrap();
        assert_eq!(
            facts,
            ModelBillingFacts::OpenAi(OpenAiBillingFacts::default())
        );
    }

    fn openai_pricing_json(tag: &str, tag_value: &str) -> serde_json::Value {
        let mut value = serde_json::json!({
            "input": { "usd_micros": 1 },
            "cached_input": null,
            "output": { "usd_micros": 2 }
        });
        value
            .as_object_mut()
            .unwrap()
            .insert(tag.to_string(), tag_value.into());
        value
    }
}
