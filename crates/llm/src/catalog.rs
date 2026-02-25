use crate::types::ModelInfo;
use std::sync::LazyLock;

/// Built-in model catalog loaded from catalog.json (Section 2.9).
/// The catalog is advisory, not restrictive -- unknown model strings pass through.
static BUILT_IN_MODELS: LazyLock<Vec<ModelInfo>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("catalog.json"))
        .expect("embedded catalog.json must be valid")
});

/// Get model info by model ID (Section 2.9).
#[must_use]
pub fn get_model_info(model_id: &str) -> Option<ModelInfo> {
    BUILT_IN_MODELS
        .iter()
        .find(|m| m.id == model_id || m.aliases.iter().any(|a| a == model_id))
        .cloned()
}

/// List all known models, optionally filtered by provider (Section 2.9).
#[must_use]
pub fn list_models(provider: Option<&str>) -> Vec<ModelInfo> {
    provider.map_or_else(
        || BUILT_IN_MODELS.clone(),
        |p| BUILT_IN_MODELS.iter().filter(|m| m.provider == p).cloned().collect(),
    )
}

/// Get the latest/best model for a provider, optionally filtered by capability (Section 2.9).
#[must_use]
pub fn get_latest_model(provider: &str, capability: Option<&str>) -> Option<ModelInfo> {
    let mut models = BUILT_IN_MODELS.iter().filter(|m| m.provider == provider);

    match capability {
        Some("reasoning") => models.find(|m| m.supports_reasoning).cloned(),
        Some("vision") => models.find(|m| m.supports_vision).cloned(),
        Some("tools") => models.find(|m| m.supports_tools).cloned(),
        _ => models.next().cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_model_info_by_id() {
        let info = get_model_info("claude-opus-4-6").unwrap();
        assert_eq!(info.display_name, "Claude Opus 4.6");
        assert_eq!(info.provider, "anthropic");
        assert!(info.supports_tools);
        assert!(info.supports_vision);
        assert!(info.supports_reasoning);
        assert_eq!(info.context_window, 1_000_000);
        assert_eq!(info.max_output, Some(128_000));
    }

    #[test]
    fn get_model_info_by_alias() {
        let info = get_model_info("opus").unwrap();
        assert_eq!(info.id, "claude-opus-4-6");

        let info = get_model_info("sonnet").unwrap();
        assert_eq!(info.id, "claude-sonnet-4-5");

        let info = get_model_info("codex").unwrap();
        assert_eq!(info.id, "gpt-5.2-codex");
    }

    #[test]
    fn get_model_info_returns_none_for_unknown() {
        assert!(get_model_info("nonexistent-model").is_none());
    }

    #[test]
    fn list_models_all() {
        let models = list_models(None);
        assert_eq!(models.len(), 8);
    }

    #[test]
    fn list_models_by_provider() {
        let anthropic = list_models(Some("anthropic"));
        assert_eq!(anthropic.len(), 3);
        assert!(anthropic.iter().all(|m| m.provider == "anthropic"));

        let openai = list_models(Some("openai"));
        assert_eq!(openai.len(), 3);

        let gemini = list_models(Some("gemini"));
        assert_eq!(gemini.len(), 2);

        let unknown = list_models(Some("unknown"));
        assert!(unknown.is_empty());
    }

    #[test]
    fn get_latest_model_returns_first_for_provider() {
        let model = get_latest_model("anthropic", None).unwrap();
        assert_eq!(model.id, "claude-opus-4-6");

        let model = get_latest_model("openai", None).unwrap();
        assert_eq!(model.id, "gpt-5.2");

        let model = get_latest_model("gemini", None).unwrap();
        assert_eq!(model.id, "gemini-3.1-pro-preview");
    }

    #[test]
    fn get_latest_model_filtered_by_capability() {
        let model = get_latest_model("anthropic", Some("reasoning")).unwrap();
        assert!(model.supports_reasoning);

        let model = get_latest_model("openai", Some("vision")).unwrap();
        assert!(model.supports_vision);

        let model = get_latest_model("gemini", Some("tools")).unwrap();
        assert!(model.supports_tools);
    }

    #[test]
    fn get_latest_model_returns_none_for_unknown_provider() {
        assert!(get_latest_model("unknown", None).is_none());
    }

    #[test]
    fn model_info_costs() {
        let claude = get_model_info("claude-opus-4-6").unwrap();
        assert_eq!(claude.input_cost_per_million, Some(15.0));
        assert_eq!(claude.output_cost_per_million, Some(75.0));

        let sonnet = get_model_info("claude-sonnet-4-5").unwrap();
        assert_eq!(sonnet.input_cost_per_million, Some(3.0));
    }
}