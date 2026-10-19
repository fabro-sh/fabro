//! Model selection shared by every Fabro dispatch boundary.
//!
//! lithos resolves a request's selector at call time. Fabro also has to pick
//! a provider and model before there is a request: when a run is created,
//! when a workflow is validated, when a fallback chain is compiled. Those
//! boundaries share one passthrough policy:
//!
//! - A provider id or alias without an explicit pin selects that provider's
//!   default.
//! - A selector known to the catalog resolves to its canonical offering.
//! - `provider/model` pins the provider, as the lithos resolver reads it.
//! - An unknown selector pinned to a provider passes through verbatim on that
//!   provider.
//! - An unqualified unknown selector passes through on the default provider.
//! - No selector picks the default offering (of the pinned provider, when one
//!   is given).
//!
//! Only enabled providers take part. Disabled ones are invisible here,
//! exactly as they are to the lithos resolver at request time.

use std::collections::HashSet;
use std::fmt;

use lithos_llm::catalog::{Catalog, ModelId, Offering, ProviderId};
use thiserror::Error;

/// A provider/model pair one of the selection functions chose.
///
/// `model` is the canonical catalog id when the selector matched an offering,
/// or the caller's selector passed through verbatim when it did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedModel {
    pub provider: ProviderId,
    pub model:    String,
}

/// A resolved fallback target: provider id plus model id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackTarget {
    pub provider: ProviderId,
    pub model:    ModelId,
}

impl FallbackTarget {
    /// Builds a target from anything that renders as a provider id and model
    /// id, so callers holding typed ids or bare passthrough selectors all use
    /// one constructor.
    pub fn new(provider: impl fmt::Display, model: impl fmt::Display) -> Self {
        Self {
            provider: ProviderId::new(provider.to_string()),
            model:    ModelId::new(model.to_string()),
        }
    }
}

impl fmt::Display for FallbackTarget {
    /// Renders as `provider:model`, the qualified form model references
    /// accept.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.provider, self.model)
    }
}

/// Why a selection could not be made.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ModelSelectionError {
    #[error("unknown model provider '{provider}'")]
    UnknownProvider { provider: String },
    #[error("model provider '{provider}' is unavailable")]
    ProviderUnavailable { provider: ProviderId },
    #[error("unknown model selector '{selector}'")]
    UnknownSelector { selector: String },
    #[error("model selector '{selector}' is unknown on provider '{provider}'")]
    UnknownSelectorOnProvider {
        selector: String,
        provider: ProviderId,
    },
    #[error(
        "model selector '{selector}' is known but has no offering on an eligible provider; available providers: {providers:?}"
    )]
    NoEligibleOffering {
        selector:  String,
        providers: Vec<ProviderId>,
    },
    #[error(
        "no default model is available on an eligible provider; providers with defaults: {providers:?}"
    )]
    NoDefaultModel { providers: Vec<ProviderId> },
}

/// Canonicalizes a provider id or alias, requiring an enabled provider.
pub fn require_provider(
    catalog: &Catalog,
    selector: &str,
) -> Result<ProviderId, ModelSelectionError> {
    catalog
        .enabled_provider(selector)
        .map(|provider| provider.id().clone())
        .ok_or_else(|| ModelSelectionError::UnknownProvider {
            provider: selector.to_string(),
        })
}

/// Canonicalizes a provider and requires it to be in the eligible set.
pub fn ready_provider(
    catalog: &Catalog,
    provider: &ProviderId,
    eligible: &HashSet<ProviderId>,
) -> Result<ProviderId, ModelSelectionError> {
    let provider = require_provider(catalog, provider.as_str())?;
    if canonical_eligible(catalog, eligible).contains(&provider) {
        Ok(provider)
    } else {
        Err(ModelSelectionError::ProviderUnavailable { provider })
    }
}

/// Finds `selector` as a model on an enabled provider.
pub fn resolve_on_provider<'a>(
    catalog: &'a Catalog,
    provider: &ProviderId,
    selector: &str,
) -> Result<Offering<'a>, ModelSelectionError> {
    let provider = require_provider(catalog, provider.as_str())?;
    catalog
        .enabled_provider(provider.as_str())
        .and_then(|provider| provider.offering(selector))
        .ok_or(ModelSelectionError::UnknownSelectorOnProvider {
            selector: selector.to_string(),
            provider,
        })
}

/// Selects a catalog model for `selector`, requiring a real offering.
///
/// With an explicit provider the model must exist there. Otherwise models
/// named `selector` are preferred over aliases, and the highest-priority
/// eligible offering wins.
pub fn select<'a>(
    catalog: &'a Catalog,
    selector: &str,
    explicit_provider: Option<&ProviderId>,
    eligible: &HashSet<ProviderId>,
) -> Result<Offering<'a>, ModelSelectionError> {
    if let Some(explicit) = explicit_provider {
        let provider = ready_provider(catalog, explicit, eligible)?;
        return resolve_on_provider(catalog, &provider, selector);
    }
    // `provider/model` pins the provider, exactly as the lithos resolver reads
    // it at request time. A slash whose prefix is not a provider (an
    // aggregator's `vendor/model` api id) falls through to plain matching.
    if let Some((prefix, rest)) = selector.split_once('/') {
        if let Some(provider) = catalog.enabled_provider(prefix) {
            let provider = ready_provider(catalog, provider.id(), eligible)?;
            return resolve_on_provider(catalog, &provider, rest);
        }
    }
    let matches = catalog.offerings_matching(selector);
    if matches.is_empty() {
        return Err(ModelSelectionError::UnknownSelector {
            selector: selector.to_string(),
        });
    }
    let eligible = canonical_eligible(catalog, eligible);
    let providers: Vec<ProviderId> = matches
        .iter()
        .map(|entry| entry.provider.id().clone())
        .collect();
    matches
        .into_iter()
        .find(|entry| eligible.contains(entry.provider.id()))
        .ok_or(ModelSelectionError::NoEligibleOffering {
            selector: selector.to_string(),
            providers,
        })
}

/// The default offering of the highest-priority eligible provider.
pub fn select_default<'a>(
    catalog: &'a Catalog,
    eligible: &HashSet<ProviderId>,
) -> Result<Offering<'a>, ModelSelectionError> {
    let eligible = canonical_eligible(catalog, eligible);
    let providers_with_defaults: Vec<_> = catalog
        .enabled_providers()
        .into_iter()
        .filter_map(|provider| {
            provider
                .default_offering()
                .map(|offering| (provider.id().clone(), offering))
        })
        .collect();
    providers_with_defaults
        .iter()
        .find(|(provider, _)| eligible.contains(provider))
        .map(|(_, offering)| *offering)
        .ok_or_else(|| ModelSelectionError::NoDefaultModel {
            providers: providers_with_defaults
                .into_iter()
                .map(|(provider, _)| provider)
                .collect(),
        })
}

/// Resolves an optional selector to one provider/model pair under Fabro's
/// passthrough policy (see the module docs).
pub fn resolve_selection(
    catalog: &Catalog,
    selector: Option<&str>,
    explicit_provider: Option<&ProviderId>,
    eligible: &HashSet<ProviderId>,
) -> Result<SelectedModel, ModelSelectionError> {
    // Provider-only selectors use the same default and readiness checks as
    // an explicit provider with no model. Recognize disabled providers too:
    // they must be rejected, not passed through to another provider as models.
    let provider_selector = selector
        .filter(|_| explicit_provider.is_none())
        .and_then(|selector| catalog.provider(selector).ok());
    let (selector, explicit_provider) = match provider_selector {
        Some(provider) => (None, Some(provider.id())),
        None => (selector, explicit_provider),
    };
    let Some(selector) = selector else {
        let eligible = match explicit_provider {
            Some(provider) => HashSet::from([ready_provider(catalog, provider, eligible)?]),
            None => eligible.clone(),
        };
        let offering = select_default(catalog, &eligible)?;
        return Ok(SelectedModel {
            provider: offering.provider.id().clone(),
            model:    offering.model.id().to_string(),
        });
    };
    match select(catalog, selector, explicit_provider, eligible) {
        Ok(offering) => Ok(SelectedModel {
            provider: offering.provider.id().clone(),
            model:    offering.model.id().to_string(),
        }),
        Err(ModelSelectionError::UnknownSelectorOnProvider { provider, selector }) => {
            Ok(SelectedModel {
                provider,
                model: selector,
            })
        }
        Err(ModelSelectionError::UnknownSelector { .. }) => {
            let default = select_default(catalog, eligible)?;
            Ok(SelectedModel {
                provider: default.provider.id().clone(),
                model:    selector.to_string(),
            })
        }
        Err(error) => Err(error),
    }
}

/// Resolves against `preferred` providers first, falling back to every enabled
/// provider only when the preferred set cannot supply the requested provider
/// or model. Semantic failures such as an unknown provider do not fall back.
pub fn resolve_selection_with_catalog_fallback(
    catalog: &Catalog,
    selector: Option<&str>,
    explicit_provider: Option<&ProviderId>,
    preferred: &HashSet<ProviderId>,
) -> Result<SelectedModel, ModelSelectionError> {
    match resolve_selection(catalog, selector, explicit_provider, preferred) {
        Err(
            ModelSelectionError::ProviderUnavailable { .. }
            | ModelSelectionError::NoEligibleOffering { .. }
            | ModelSelectionError::NoDefaultModel { .. },
        ) => resolve_selection(
            catalog,
            selector,
            explicit_provider,
            &catalog.enabled_provider_ids().into_iter().collect(),
        ),
        result => result,
    }
}

fn canonical_eligible(catalog: &Catalog, eligible: &HashSet<ProviderId>) -> HashSet<ProviderId> {
    eligible
        .iter()
        .filter_map(|id| catalog.enabled_provider(id.as_str()))
        .map(|provider| provider.id().clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use lithos_llm::catalog::builtin;

    use super::*;
    use crate::test_support::{test_catalog, test_catalog_with_overlay};

    fn eligible(ids: &[&str]) -> HashSet<ProviderId> {
        ids.iter().map(|id| ProviderId::new(*id)).collect()
    }

    #[test]
    fn known_alias_resolves_to_canonical_offering_on_an_eligible_provider() {
        let catalog = test_catalog();
        let selected =
            resolve_selection(&catalog, Some("sonnet"), None, &eligible(&["anthropic"])).unwrap();
        assert_eq!(selected, SelectedModel {
            provider: builtin::anthropic(),
            model:    "claude-sonnet-5".to_string(),
        });
    }

    #[test]
    fn provider_selectors_and_aliases_use_the_configured_default() {
        let catalog = test_catalog_with_overlay(
            "[providers.gemini]\ndefault_model = \"gemini-3.1-flash-lite\"\n",
        );
        for selector in ["gemini", "google"] {
            let selected = resolve_selection(
                &catalog,
                Some(selector),
                None,
                &eligible(&["anthropic", "gemini"]),
            )
            .unwrap();
            assert_eq!(selected, SelectedModel {
                provider: builtin::gemini(),
                model:    "gemini-3.1-flash-lite".to_string(),
            });
        }
    }

    #[test]
    fn unavailable_provider_selector_does_not_fall_back() {
        let catalog = test_catalog();
        for selector in ["gemini", "google"] {
            assert_eq!(
                resolve_selection(&catalog, Some(selector), None, &eligible(&["anthropic"])),
                Err(ModelSelectionError::ProviderUnavailable {
                    provider: builtin::gemini(),
                })
            );
        }
    }

    #[test]
    fn disabled_provider_selector_is_rejected_instead_of_passed_through() {
        let catalog = test_catalog_with_overlay("[providers.gemini]\nenabled = false\n");
        assert_eq!(
            resolve_selection(&catalog, Some("gemini"), None, &eligible(&["anthropic"])),
            Err(ModelSelectionError::UnknownProvider {
                provider: "gemini".to_string(),
            })
        );
    }

    #[test]
    fn provider_without_a_default_does_not_use_another_providers_default() {
        let catalog = test_catalog_with_overlay(
            r#"
[providers.custom]
display_name = "Custom"
base_url = "http://127.0.0.1:1"
auth = { type = "none" }
[providers.custom.models.example]
display_name = "Example"
api_model = "example"
"#,
        );
        assert!(matches!(
            resolve_selection(
                &catalog,
                Some("custom"),
                None,
                &eligible(&["anthropic", "custom"])
            ),
            Err(ModelSelectionError::NoDefaultModel { .. })
        ));
    }

    #[test]
    fn explicit_provider_preserves_literal_models_named_after_a_provider() {
        let catalog = test_catalog();
        let selected = resolve_selection(
            &catalog,
            Some("gemini"),
            Some(&builtin::anthropic()),
            &eligible(&["anthropic", "gemini"]),
        )
        .unwrap();
        assert_eq!(selected, SelectedModel {
            provider: builtin::anthropic(),
            model:    "gemini".to_string(),
        });
    }

    #[test]
    fn unknown_selector_passes_through_on_the_default_provider() {
        let catalog = test_catalog();
        let selected = resolve_selection(
            &catalog,
            Some("totally-new-model"),
            None,
            &eligible(&["openai", "anthropic"]),
        )
        .unwrap();
        assert_eq!(selected.provider, builtin::anthropic());
        assert_eq!(selected.model, "totally-new-model");
    }

    #[test]
    fn slash_qualified_selector_pins_the_provider_like_the_lithos_resolver() {
        let catalog = test_catalog();
        let selected = resolve_selection(
            &catalog,
            Some("openai/gpt-5.6-sol"),
            None,
            &eligible(&["openai", "anthropic"]),
        )
        .unwrap();
        assert_eq!(selected, SelectedModel {
            provider: builtin::openai(),
            model:    "gpt-5.6-sol".to_string(),
        });

        let unknown = resolve_selection(
            &catalog,
            Some("openai/brand-new-model"),
            None,
            &eligible(&["openai", "anthropic"]),
        )
        .unwrap();
        assert_eq!(unknown, SelectedModel {
            provider: builtin::openai(),
            model:    "brand-new-model".to_string(),
        });

        let unavailable = resolve_selection(
            &catalog,
            Some("openai/gpt-5.6-sol"),
            None,
            &eligible(&["anthropic"]),
        );
        assert_eq!(
            unavailable,
            Err(ModelSelectionError::ProviderUnavailable {
                provider: builtin::openai(),
            })
        );
    }

    #[test]
    fn slash_selector_with_a_non_provider_prefix_matches_api_ids_on_a_pinned_provider() {
        let catalog = test_catalog_with_overlay("[providers.openrouter]\nenabled = true\n");
        let selected = resolve_selection(
            &catalog,
            Some("openai/gpt-5.6-sol"),
            Some(&ProviderId::new("openrouter")),
            &eligible(&["openrouter"]),
        )
        .unwrap();
        assert_eq!(selected, SelectedModel {
            provider: ProviderId::new("openrouter"),
            model:    "gpt-5.6-sol".to_string(),
        });
    }

    #[test]
    fn pinned_provider_must_be_eligible() {
        let catalog = test_catalog();
        let error = resolve_selection(
            &catalog,
            Some("gpt-5.4"),
            Some(&builtin::openai()),
            &eligible(&["anthropic"]),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ModelSelectionError::ProviderUnavailable { provider } if provider == builtin::openai()
        ));
    }

    #[test]
    fn catalog_fallback_recovers_from_readiness_failures_only() {
        let catalog = test_catalog();
        let selected = resolve_selection_with_catalog_fallback(
            &catalog,
            Some("gpt-5.4"),
            Some(&builtin::openai()),
            &eligible(&["anthropic"]),
        )
        .unwrap();
        assert_eq!(selected.provider, builtin::openai());
        let error = resolve_selection_with_catalog_fallback(
            &catalog,
            None,
            Some(&ProviderId::new("nope")),
            &eligible(&["anthropic"]),
        )
        .unwrap_err();
        assert!(matches!(error, ModelSelectionError::UnknownProvider { .. }));
    }

    #[test]
    fn disabled_providers_are_not_selectable() {
        let catalog = test_catalog();
        assert!(matches!(
            select(&catalog, "gpt-5.4", None, &eligible(&["bedrock-openai"])),
            Err(ModelSelectionError::NoEligibleOffering { .. })
        ));
        let enabled = test_catalog_with_overlay("[providers.bedrock-openai]\nenabled = true\n");
        let entry = select(&enabled, "gpt-5.4", None, &eligible(&["bedrock-openai"])).unwrap();
        assert_eq!(entry.provider.id(), &ProviderId::new("bedrock-openai"));
    }

    #[test]
    fn fallback_targets_render_qualified() {
        assert_eq!(
            FallbackTarget::new("openai", "gpt-5.4").to_string(),
            "openai:gpt-5.4"
        );
    }
}
