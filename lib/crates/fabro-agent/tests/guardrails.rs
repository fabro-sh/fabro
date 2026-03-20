use fabro_agent::{AnthropicProfile, GeminiProfile, OpenAiProfile, ProviderProfile};
use fabro_model as catalog;
use fabro_model::Provider;

#[test]
fn profile_context_window_matches_catalog_for_default_models() {
    for &provider in Provider::ALL {
        let catalog_info = catalog::default_model_for_provider(provider.as_str())
            .unwrap_or_else(|| panic!("no default model for {:?} in catalog", provider));
        let model = &catalog_info.id;

        let profile: Box<dyn ProviderProfile> = match provider {
            Provider::OpenAi => Box::new(OpenAiProfile::new(model)),
            Provider::Kimi | Provider::Zai | Provider::Minimax | Provider::Inception => {
                Box::new(OpenAiProfile::new(model).with_provider(provider))
            }
            Provider::Gemini => Box::new(GeminiProfile::new(model)),
            Provider::Anthropic => Box::new(AnthropicProfile::new(model)),
        };

        assert_eq!(
            profile.context_window_size(),
            catalog_info.limits.context_window as usize,
            "context_window_size mismatch for {:?} model '{}': profile={} catalog={}",
            provider,
            model,
            profile.context_window_size(),
            catalog_info.limits.context_window as usize
        );
    }
}
