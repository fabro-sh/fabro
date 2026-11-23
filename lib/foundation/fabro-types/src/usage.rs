//! Usage vocabulary.
//!
//! Token counts and cost come from lithos: [`Usage`] holds the five disjoint
//! token buckets ([`TokenCounts`]) and, when known, what they cost
//! ([`Cost`], with the [`CostSource`] it came from). Fabro sums that usage
//! across responses, stages, and runs with [`Usage::saturating_add`]. The
//! types here are [`ModelRef`], the identity a usage is grouped under, and
//! [`ModelUsage`], a usage with that identity.
//!
//! [`TokenCounts`]: lithos_llm::types::TokenCounts
//! [`Cost`]: lithos_llm::types::Cost
//! [`CostSource`]: lithos_llm::types::CostSource

use lithos_llm::catalog::{ModelHandle, ModelId, ProviderId};
use lithos_llm::types::{Speed, Usage};
use serde::{Deserialize, Serialize};

/// Provider-qualified model identity a usage is grouped under.
///
/// Carries the requested speed tier because providers price tiers
/// differently, so two responses from the same model at different speeds are
/// separate usage rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: ProviderId,
    pub model_id: ModelId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed:    Option<Speed>,
}

impl ModelRef {
    #[must_use]
    pub fn new(provider: ProviderId, model_id: ModelId) -> Self {
        Self {
            provider,
            model_id,
            speed: None,
        }
    }

    #[must_use]
    pub fn from_handle(handle: &ModelHandle, speed: Option<Speed>) -> Self {
        Self {
            provider: handle.provider().clone(),
            model_id: handle.model().clone(),
            speed,
        }
    }

    #[must_use]
    pub fn with_speed(mut self, speed: Option<Speed>) -> Self {
        self.speed = speed;
        self
    }

    #[must_use]
    pub fn handle(&self) -> ModelHandle {
        ModelHandle::new(self.provider.clone(), self.model_id.clone())
    }

    /// Stable ordering key: provider, then model, then speed label.
    #[must_use]
    pub fn sort_key(&self) -> (&str, &str, &'static str) {
        (
            self.provider.as_str(),
            self.model_id.as_str(),
            self.speed.map_or("", Speed::as_str),
        )
    }
}

impl std::hash::Hash for ModelRef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.provider.hash(state);
        self.model_id.hash(state);
        self.speed.map(Speed::as_str).hash(state);
    }
}

impl std::fmt::Display for ModelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.provider, self.model_id)?;
        if let Some(speed) = self.speed {
            write!(f, " ({speed})")?;
        }
        Ok(())
    }
}

/// Usage grouped under one model: one response, or one model's share of a
/// stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    pub model: ModelRef,
    #[serde(default)]
    pub usage: Usage,
}

impl ModelUsage {
    #[must_use]
    pub fn new(model: ModelRef, usage: Usage) -> Self {
        Self { model, usage }
    }

    #[must_use]
    pub fn model(&self) -> &ModelRef {
        &self.model
    }

    #[must_use]
    pub fn model_id(&self) -> &str {
        self.model.model_id.as_str()
    }
}

/// Sums usages with [`Usage::saturating_add`]; the empty sum is
/// [`Usage::default`].
pub fn sum_usage(usages: impl IntoIterator<Item = Usage>) -> Usage {
    usages
        .into_iter()
        .fold(Usage::default(), Usage::saturating_add)
}

/// No tokens and no cost data: the usage of something that made no model
/// calls.
#[must_use]
pub fn usage_is_empty(usage: &Usage) -> bool {
    *usage == Usage::default()
}

/// Format a USD cost for display, to the cent.
#[must_use]
pub fn format_cost(cost: f64) -> String {
    format!("${cost:.2}")
}

#[cfg(test)]
mod tests {
    use lithos_llm::types::{Cost, CostSource, TokenCounts};
    use serde_json::json;

    use super::*;

    fn tokens() -> TokenCounts {
        TokenCounts {
            input:            100,
            output:           20,
            reasoning:        5,
            cache_read:       7,
            cache_write:      3,
            long_cache_write: 2,
        }
    }

    fn model() -> ModelRef {
        ModelRef::new(
            ProviderId::new("anthropic"),
            ModelId::new("claude-sonnet-5"),
        )
    }

    #[test]
    fn sum_usage_adds_tokens_and_keeps_a_shared_cost_source() {
        let priced = Usage {
            tokens: tokens(),
            cost:   Some(Cost {
                usd_micros: 10,
                source:     CostSource::Catalog,
            }),
        };
        let total = sum_usage([priced, priced, Usage::default()]);
        assert_eq!(total.tokens.input, 200);
        assert_eq!(total.tokens.cache_write, 6);
        assert_eq!(total.tokens.long_cache_write, 4);
        assert_eq!(total.tokens.total(), 270);
        assert_eq!(
            total.cost,
            Some(Cost {
                usd_micros: 20,
                source:     CostSource::Catalog,
            })
        );
    }

    #[test]
    fn sum_usage_drops_the_cost_once_an_unpriced_part_used_tokens() {
        let priced = Usage {
            tokens: tokens(),
            cost:   Some(Cost {
                usd_micros: 10,
                source:     CostSource::Catalog,
            }),
        };
        let unpriced = Usage::from(tokens());
        let total = sum_usage([priced, unpriced]);
        assert_eq!(total.tokens.input, 200);
        assert_eq!(total.cost, None);
        assert_eq!(sum_usage([]), Usage::default());
    }

    #[test]
    fn usage_is_empty_only_without_tokens_and_cost() {
        assert!(usage_is_empty(&Usage::default()));
        assert!(!usage_is_empty(&Usage::from(tokens())));
        assert!(!usage_is_empty(&Usage {
            tokens: TokenCounts::default(),
            cost:   Some(Cost {
                usd_micros: 1,
                source:     CostSource::Provider,
            }),
        }));
    }

    #[test]
    fn model_usage_serializes_lithos_usage_shape() {
        let usage = ModelUsage::new(model().with_speed(Some(Speed::Fast)), Usage {
            tokens: tokens(),
            cost:   Some(Cost {
                usd_micros: 42,
                source:     CostSource::Provider,
            }),
        });
        let value = serde_json::to_value(&usage).unwrap();
        assert_eq!(
            value,
            json!({
                "model": {
                    "provider": "anthropic",
                    "model_id": "claude-sonnet-5",
                    "speed": "fast",
                },
                "usage": {
                    "tokens": {
                        "input": 100,
                        "output": 20,
                        "reasoning": 5,
                        "cache_read": 7,
                        "cache_write": 3,
                        "long_cache_write": 2,
                    },
                    "cost": { "usd_micros": 42, "source": "provider" },
                },
            })
        );
        let back: ModelUsage = serde_json::from_value(value).unwrap();
        assert_eq!(back, usage);
    }

    #[test]
    fn model_usage_omits_an_absent_cost() {
        let usage = ModelUsage::new(model(), Usage::from(tokens()));
        let value = serde_json::to_value(&usage).unwrap();
        assert_eq!(value["usage"].get("cost"), None);
        let back: ModelUsage = serde_json::from_value(value).unwrap();
        assert_eq!(back, usage);
    }

    #[test]
    fn model_ref_hash_distinguishes_speed_tiers() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(model());
        set.insert(model().with_speed(Some(Speed::Fast)));
        set.insert(model().with_speed(Some(Speed::Fast)));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn model_ref_display_names_the_route_and_speed() {
        assert_eq!(model().to_string(), "anthropic/claude-sonnet-5");
        assert_eq!(
            model().with_speed(Some(Speed::Fast)).to_string(),
            "anthropic/claude-sonnet-5 (fast)"
        );
    }
}
