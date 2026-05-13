use std::sync::Arc;

use fabro_agent::{AgentProfile, AnthropicProfile, GeminiProfile, OpenAiProfile};
use fabro_model::catalog::LlmCatalogSettings;
use fabro_model::{Catalog, Provider};

#[test]
fn profile_context_window_matches_catalog_for_default_models() {
    let catalog =
        Arc::new(Catalog::from_builtin_with_overrides(&LlmCatalogSettings::default()).unwrap());
    for &provider in Provider::ALL {
        let catalog_info = catalog
            .default_for_provider(&provider.id())
            .cloned()
            .unwrap_or_else(|| panic!("no default model for {provider:?} in catalog"));
        let model = &catalog_info.id;
        let context_window = usize::try_from(catalog_info.context_window())
            .expect("catalog context window should be non-negative and fit in usize");

        let profile: Box<dyn AgentProfile> = match provider {
            Provider::OpenAi => {
                Box::new(OpenAiProfile::new(model).with_catalog(Arc::clone(&catalog)))
            }
            Provider::Kimi
            | Provider::Zai
            | Provider::Minimax
            | Provider::Inception
            | Provider::OpenAiCompatible => Box::new(
                OpenAiProfile::new(model)
                    .with_provider(provider)
                    .with_catalog(Arc::clone(&catalog)),
            ),
            Provider::Gemini => {
                Box::new(GeminiProfile::new(model).with_catalog(Arc::clone(&catalog)))
            }
            Provider::Anthropic => {
                Box::new(AnthropicProfile::new(model).with_catalog(Arc::clone(&catalog)))
            }
        };

        assert_eq!(
            profile.context_window_size(),
            context_window,
            "context_window_size mismatch for {:?} model '{}': profile={} catalog={}",
            provider,
            model,
            profile.context_window_size(),
            context_window
        );
    }
}
