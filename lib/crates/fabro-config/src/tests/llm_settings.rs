use crate::{CredentialRef, SettingsLayer};

#[test]
fn parses_llm_provider_settings() {
    let input = r#"
_version = 1

[llm.providers.kimi]
display_name = "Kimi"
adapter = "openai_compatible"
base_url = "https://api.moonshot.ai/v1"
credentials = ["credential:kimi", "env:KIMI_API_KEY"]
priority = 60
enabled = true
aliases = ["moonshot"]
"#;

    let layer: SettingsLayer = input.parse().unwrap();
    let llm = layer.llm.unwrap();
    let kimi = llm.providers.get("kimi").unwrap();

    assert_eq!(kimi.display_name.as_deref(), Some("Kimi"));
    assert_eq!(kimi.adapter.as_deref(), Some("openai_compatible"));
    assert_eq!(kimi.base_url.as_deref(), Some("https://api.moonshot.ai/v1"));
    assert_eq!(kimi.credentials.len(), 2);
    assert_eq!(
        kimi.credentials[0],
        CredentialRef::Credential("kimi".to_string())
    );
    assert_eq!(
        kimi.credentials[1],
        CredentialRef::Env("KIMI_API_KEY".to_string())
    );
    assert_eq!(kimi.priority, Some(60));
    assert_eq!(kimi.enabled, Some(true));
    assert_eq!(kimi.aliases, vec!["moonshot"]);
}

#[test]
fn parses_llm_model_settings() {
    let input = r#"
_version = 1

[llm.models."kimi-k2.5"]
provider = "kimi"
api_id = "kimi-k2.5"
display_name = "Kimi K2.5"
family = "kimi"
knowledge_cutoff = 2025-01-01
default = true
enabled = true
aliases = ["kimi"]
estimated_output_tps = 50.0

[llm.models."kimi-k2.5".limits]
context_window = 262144
max_output = 32768

[llm.models."kimi-k2.5".features]
tools = true
vision = false
reasoning = true
effort = false

[llm.models."kimi-k2.5".costs]
input_cost_per_mtok = 0.60
output_cost_per_mtok = 2.50
cache_input_cost_per_mtok = 0.15

[llm.models."kimi-k2.5".controls]
reasoning_effort = ["low", "medium", "high"]
"#;

    let layer: SettingsLayer = input.parse().unwrap();
    let llm = layer.llm.unwrap();
    let model = llm.models.get("kimi-k2.5").unwrap();

    assert_eq!(model.provider.as_deref(), Some("kimi"));
    assert_eq!(model.api_id.as_deref(), Some("kimi-k2.5"));
    assert_eq!(model.display_name.as_deref(), Some("Kimi K2.5"));
    assert_eq!(model.family.as_deref(), Some("kimi"));
    assert_eq!(model.default, Some(true));
    assert_eq!(model.enabled, Some(true));
    assert_eq!(model.aliases, vec!["kimi"]);
    assert_eq!(model.estimated_output_tps, Some(50.0));

    let limits = model.limits.as_ref().unwrap();
    assert_eq!(limits.context_window, Some(262_144));
    assert_eq!(limits.max_output, Some(32_768));

    let features = model.features.as_ref().unwrap();
    assert_eq!(features.tools, Some(true));
    assert_eq!(features.vision, Some(false));
    assert_eq!(features.reasoning, Some(true));
    assert_eq!(features.effort, Some(false));

    let costs = model.costs.as_ref().unwrap();
    assert_eq!(costs.input_cost_per_mtok, Some(0.60));
    assert_eq!(costs.output_cost_per_mtok, Some(2.50));
    assert_eq!(costs.cache_input_cost_per_mtok, Some(0.15));

    let controls = model.controls.as_ref().unwrap();
    assert_eq!(controls.reasoning_effort, vec!["low", "medium", "high"]);
}

#[test]
fn parses_model_speed_costs() {
    let input = r#"
_version = 1

[llm.models."claude-opus-4-6".costs]
input_cost_per_mtok = 5.0
output_cost_per_mtok = 25.0
cache_input_cost_per_mtok = 0.5

[llm.models."claude-opus-4-6".costs.speed.fast]
input_cost_per_mtok = 30.0
output_cost_per_mtok = 150.0
cache_input_cost_per_mtok = 3.0

[llm.models."claude-opus-4-6".controls]
reasoning_effort = ["low", "medium", "high"]
speed = ["fast"]
"#;

    let layer: SettingsLayer = input.parse().unwrap();
    let model = layer
        .llm
        .unwrap()
        .models
        .into_inner()
        .remove("claude-opus-4-6")
        .unwrap();

    let costs = model.costs.unwrap();
    assert_eq!(costs.input_cost_per_mtok, Some(5.0));
    let fast_costs = &costs.speed["fast"];
    assert_eq!(fast_costs.input_cost_per_mtok, Some(30.0));
    assert_eq!(fast_costs.output_cost_per_mtok, Some(150.0));

    let controls = model.controls.unwrap();
    assert_eq!(controls.speed, vec!["fast"]);
}

#[test]
fn rejects_literal_credential_secret() {
    let input = r#"
_version = 1

[llm.providers.custom]
adapter = "openai_compatible"
credentials = ["sk-secret-key-literal"]
"#;

    let err = input.parse::<SettingsLayer>().unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("credential:"),
        "error should mention credential: prefix, got: {msg}"
    );
}

#[test]
fn rejects_empty_credential_id() {
    let input = r#"
_version = 1

[llm.providers.custom]
credentials = ["credential:"]
"#;

    let err = input.parse::<SettingsLayer>().unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("non-empty"),
        "error should mention non-empty, got: {msg}"
    );
}

#[test]
fn rejects_empty_env_name() {
    let input = r#"
_version = 1

[llm.providers.custom]
credentials = ["env:"]
"#;

    let err = input.parse::<SettingsLayer>().unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("non-empty"),
        "error should mention non-empty, got: {msg}"
    );
}

#[test]
fn llm_top_level_key_accepted() {
    let input = r#"
_version = 1

[llm.providers.test]
adapter = "openai_compatible"
"#;

    let layer: SettingsLayer = input.parse().unwrap();
    assert!(layer.llm.is_some());
}

#[test]
fn rejects_unknown_field_under_llm_providers() {
    let input = r#"
_version = 1

[llm.providers.test]
adapter = "openai_compatible"
unknown_field = "value"
"#;

    let err = input.parse::<SettingsLayer>().unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unknown"),
        "error should mention unknown field, got: {msg}"
    );
}

#[test]
fn credential_ref_display() {
    assert_eq!(
        CredentialRef::Credential("kimi".to_string()).to_string(),
        "credential:kimi"
    );
    assert_eq!(
        CredentialRef::Env("KIMI_API_KEY".to_string()).to_string(),
        "env:KIMI_API_KEY"
    );
}

#[test]
fn run_model_controls_parse() {
    let input = r#"
_version = 1

[run.model.controls]
reasoning_effort = "high"
speed = "fast"
"#;

    let layer: SettingsLayer = input.parse().unwrap();
    let controls = layer.run.unwrap().model.unwrap().controls.unwrap();
    assert_eq!(controls.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(controls.speed.as_deref(), Some("fast"));
}
