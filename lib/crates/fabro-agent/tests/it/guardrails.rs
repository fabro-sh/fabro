use std::sync::Arc;

use fabro_agent::{AgentProfile, AnthropicProfile, GeminiProfile, OpenAiProfile};
use fabro_model::{Catalog, ProviderId};

#[test]
fn profile_context_window_matches_catalog_for_default_models() {
    let catalog = Arc::new(Catalog::from_builtin().unwrap());
    for provider in catalog.providers() {
        let catalog_info = catalog
            .default_for_provider(&provider.id)
            .cloned()
            .unwrap_or_else(|| panic!("no default model for {:?} in catalog", provider.id));
        let model = &catalog_info.id;
        let context_window = usize::try_from(catalog_info.context_window())
            .expect("catalog context window should be non-negative and fit in usize");

        let profile: Box<dyn AgentProfile> = match provider.adapter.metadata().default_profile {
            fabro_model::AgentProfileKind::OpenAi if provider.id == ProviderId::openai() => {
                Box::new(OpenAiProfile::new(model).with_catalog(Arc::clone(&catalog)))
            }
            fabro_model::AgentProfileKind::OpenAi => Box::new(
                OpenAiProfile::new(model)
                    .with_provider_id(provider.id.clone())
                    .with_catalog(Arc::clone(&catalog)),
            ),
            fabro_model::AgentProfileKind::Gemini => {
                Box::new(GeminiProfile::new(model).with_catalog(Arc::clone(&catalog)))
            }
            fabro_model::AgentProfileKind::Anthropic => {
                Box::new(AnthropicProfile::new(model).with_catalog(Arc::clone(&catalog)))
            }
        };

        assert_eq!(
            profile.context_window_size(),
            context_window,
            "context_window_size mismatch for {:?} model '{}': profile={} catalog={}",
            provider.id,
            model,
            profile.context_window_size(),
            context_window
        );
    }
}
