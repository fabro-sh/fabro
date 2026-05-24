use std::any::{TypeId, type_name};

use fabro_api::types::Provider as ApiProvider;
use fabro_model::adapter::AdapterKind;
use fabro_model::{Provider, ProviderId};

#[test]
fn provider_reuses_canonical_type() {
    assert_same_type::<ApiProvider, Provider>();
}

#[test]
fn provider_json_matches_openapi_shape() {
    let provider = Provider {
        id:                   ProviderId::anthropic(),
        display_name:         "Anthropic".to_string(),
        adapter:              AdapterKind::Anthropic,
        base_url:             Some("https://api.anthropic.test/v1".to_string()),
        api_key_url:          Some("https://console.anthropic.com/settings/keys".to_string()),
        priority:             100,
        aliases:              vec!["claude".to_string()],
        model_count:          7,
        default_model:        Some("claude-opus-4-7".to_string()),
        configured:           true,
        expected_secret_name: Some("ANTHROPIC_API_KEY".to_string()),
    };

    let json = serde_json::to_value(&provider).unwrap();
    assert_eq!(json["id"], "anthropic");
    assert_eq!(json["display_name"], "Anthropic");
    assert_eq!(json["adapter"], "anthropic");
    assert_eq!(json["base_url"], "https://api.anthropic.test/v1");
    assert_eq!(
        json["api_key_url"],
        "https://console.anthropic.com/settings/keys"
    );
    assert_eq!(json["priority"], 100);
    assert_eq!(json["aliases"], serde_json::json!(["claude"]));
    assert_eq!(json["model_count"], 7);
    assert_eq!(json["default_model"], "claude-opus-4-7");
    assert_eq!(json["configured"], true);
    assert_eq!(json["expected_secret_name"], "ANTHROPIC_API_KEY");

    let round_trip: ApiProvider = serde_json::from_value(json).unwrap();
    assert_eq!(round_trip, provider);
}

#[test]
fn provider_omits_optional_fields_when_absent() {
    // Proves the required/optional split the OpenAPI `Provider` schema
    // declares: the five `skip_serializing_if` fields drop out entirely, while
    // the six required fields always serialize.
    let provider = Provider {
        id:                   ProviderId::new("custom"),
        display_name:         "Custom".to_string(),
        adapter:              AdapterKind::OpenAiCompatible,
        base_url:             None,
        api_key_url:          None,
        priority:             0,
        aliases:              Vec::new(),
        model_count:          0,
        default_model:        None,
        configured:           false,
        expected_secret_name: None,
    };

    let json = serde_json::to_value(&provider).unwrap();
    let object = json.as_object().unwrap();
    assert!(!object.contains_key("base_url"));
    assert!(!object.contains_key("api_key_url"));
    assert!(!object.contains_key("aliases"));
    assert!(!object.contains_key("default_model"));
    assert!(!object.contains_key("expected_secret_name"));
    assert!(object.contains_key("id"));
    assert!(object.contains_key("display_name"));
    assert!(object.contains_key("adapter"));
    assert!(object.contains_key("priority"));
    assert!(object.contains_key("model_count"));
    assert!(object.contains_key("configured"));

    let round_trip: ApiProvider = serde_json::from_value(json).unwrap();
    assert_eq!(round_trip, provider);
}

fn assert_same_type<T: 'static, U: 'static>() {
    assert_eq!(
        TypeId::of::<T>(),
        TypeId::of::<U>(),
        "{} should be the same type as {}",
        type_name::<T>(),
        type_name::<U>()
    );
}
