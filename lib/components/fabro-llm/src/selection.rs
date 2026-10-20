//! Fabro's boundary policy over Lithos model selection.
//!
//! Lithos owns selector parsing, defaults, aliases, priority, passthrough
//! permissions, and canonical identity. Fabro requires exact provider pins and
//! allows unknown bare models on the default provider. Install the same policy
//! on clients and use it for preflight with the actual eligible provider set.

use lithos_llm::ModelSelection;
use lithos_llm::catalog::Catalog;
use lithos_llm::resolver::{
    ProviderMatchPolicy, ResolvedRoute, SelectionPolicy, UnknownModelPolicy,
};

/// Dispatch and preflight use the same exact-provider policy.
#[must_use]
pub fn policy() -> SelectionPolicy {
    SelectionPolicy::default()
        .with_providers(ProviderMatchPolicy::Exact)
        .with_unknown_models(UnknownModelPolicy::DefaultProvider)
}

/// Workflows require explicit provider scope for unknown models. The Petri
/// client uses this during both admission and execution.
#[must_use]
pub fn workflow_policy() -> SelectionPolicy {
    policy().with_unknown_models(UnknownModelPolicy::ExplicitProvider)
}

/// Catalog inspection must never invent a passthrough catalog row.
#[must_use]
pub fn catalog_policy() -> SelectionPolicy {
    policy().with_unknown_models(UnknownModelPolicy::CatalogOnly)
}

/// Persist catalog identity exactly, or explicit scope for a passthrough route.
/// A passthrough handle alone is not a valid canonical catalog selection.
#[must_use]
pub fn for_route(catalog: &Catalog, route: &ResolvedRoute) -> ModelSelection {
    let handle = route.handle();
    if catalog
        .model(handle.provider().as_str(), handle.model().as_str())
        .is_ok()
    {
        handle.into()
    } else {
        ModelSelection::from_parts(
            Some(route.api_model()),
            Some(route.provider().id().as_str()),
        )
    }
}

#[cfg(test)]
mod tests {
    use lithos_llm::catalog::{ModelHandle, ModelId, ProviderId};
    use lithos_llm::resolver::{
        AvailableProviders, CatalogResolver, ModelResolver as _, ModelSelectionError,
    };

    use super::*;
    use crate::test_support::{test_catalog, test_catalog_with_overlay};

    #[test]
    fn exact_pins_refuse_stand_ins_and_catalog_only_refuses_unknown_models() {
        let catalog = test_catalog();
        let available = AvailableProviders::new([ProviderId::new("openai-codex")]);
        let intent = ModelSelection::from_parts(Some("openai/gpt-5.6-sol"), None);
        assert!(
            CatalogResolver
                .resolve_selection(&intent, &catalog, &available)
                .is_ok()
        );
        assert!(matches!(
            policy().resolve_selection(&intent, &catalog, &available),
            Err(ModelSelectionError::ProviderUnavailable { .. })
        ));
        let available = AvailableProviders::all(&catalog);
        let unknown = ModelSelection::from_parts(Some("future-model"), None);
        assert!(
            policy()
                .resolve_selection(&unknown, &catalog, &available)
                .is_ok()
        );
        assert!(matches!(
            catalog_policy().resolve_selection(&unknown, &catalog, &available),
            Err(ModelSelectionError::ModelNotFound { .. })
        ));
    }

    #[test]
    fn workflow_preflight_and_dispatch_require_scope_for_unknown_models() {
        let catalog = test_catalog();
        let available = AvailableProviders::all(&catalog);
        let bare = ModelSelection::from_parts(Some("future-model"), None);
        assert!(matches!(
            workflow_policy().resolve_selection(&bare, &catalog, &available),
            Err(ModelSelectionError::ModelNotFound { .. })
        ));
        let scoped = ModelSelection::from_parts(Some("future-model"), Some("openai"));
        assert!(
            workflow_policy()
                .resolve_selection(&scoped, &catalog, &available)
                .is_ok()
        );
    }

    #[test]
    fn passthrough_permission_and_provider_scope_are_enforced() {
        let catalog = test_catalog_with_overlay("[providers.openai]\nallow_passthrough = false\n");
        let available = AvailableProviders::new([ProviderId::new("openai")]);
        for intent in [
            ModelSelection::from_parts(Some("future-model"), None),
            ModelSelection::from_parts(Some("vendor/model:version"), Some("openai")),
        ] {
            assert!(matches!(
                policy().resolve_selection(&intent, &catalog, &available),
                Err(ModelSelectionError::ModelNotFound { .. })
            ));
        }
        let catalog = test_catalog();
        for model in ["vendor/model:version", "future-model:latest", "default"] {
            let intent = ModelSelection::from_parts(Some(model), Some("openai"));
            let route = policy()
                .resolve_selection(&intent, &catalog, &available)
                .unwrap();
            assert_eq!(route.api_model(), model);
        }
        for model in ["missing/model", "claude-sonnet-5"] {
            assert!(
                policy()
                    .resolve_selection(
                        &ModelSelection::from_parts(Some(model), None),
                        &catalog,
                        &available
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn disabled_unavailable_and_defaultless_providers_return_typed_errors() {
        let catalog = test_catalog_with_overlay("[providers.gemini]\nenabled = false\n");
        let available = AvailableProviders::all(&catalog);
        assert!(matches!(
            policy().resolve_selection(
                &ModelSelection::from_parts(None, Some("gemini")),
                &catalog,
                &available
            ),
            Err(ModelSelectionError::ProviderDisabled { .. })
        ));
        assert!(matches!(
            policy().resolve_selection(
                &ModelSelection::Default,
                &catalog,
                &AvailableProviders::default()
            ),
            Err(ModelSelectionError::NoDefaultModel)
        ));
        let catalog = Catalog::builder()
            .overlay_toml(
                r#"
schema_version = 1
[providers.empty]
display_name = "No default"
adapter = "http"
codecs = ["openai-responses"]
base_url = "http://127.0.0.1"
auth = { type = "none" }
"#,
            )
            .unwrap()
            .build()
            .unwrap();
        assert!(matches!(
            policy().resolve_selection(
                &ModelSelection::from_parts(None, Some("empty")),
                &catalog,
                &AvailableProviders::all(&catalog)
            ),
            Err(ModelSelectionError::ProviderHasNoDefault { .. })
        ));
    }

    #[test]
    fn canonical_persistence_refuses_removed_rows_even_when_an_alias_reuses_the_id() {
        let catalog = test_catalog();
        let handle = ModelHandle::new(ProviderId::new("openai"), ModelId::new("gpt-5.6-sol"));
        let intent: ModelSelection = handle.clone().into();
        let restored: ModelSelection =
            serde_json::from_str(&serde_json::to_string(&intent).unwrap()).unwrap();
        assert_eq!(
            policy()
                .resolve_selection(&restored, &catalog, &AvailableProviders::all(&catalog))
                .unwrap()
                .handle(),
            handle
        );
        let changed = Catalog::builder()
            .overlay_toml(
                r#"
schema_version = 1
[providers.openai]
display_name = "OpenAI"
adapter = "http"
codecs = ["openai-responses"]
base_url = "http://127.0.0.1"
auth = { type = "none" }
allow_passthrough = true
default_model = "replacement"
[providers.openai.models.replacement]
display_name = "Replacement"
api_model = "new-wire-id"
aliases = ["gpt-5.6-sol"]
"#,
            )
            .unwrap()
            .build()
            .unwrap();
        assert!(matches!(
            policy().resolve_selection(&restored, &changed, &AvailableProviders::all(&changed)),
            Err(ModelSelectionError::ModelNotFound { .. })
        ));
    }
}
