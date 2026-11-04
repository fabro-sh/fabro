use std::any::TypeId;

use fabro_api::types;
use lithos_llm::client::ProbeReport;
use lithos_llm::types::{ErrorData, TokenCounts};
use serde_json::{Value, json};

fn validator(name: &str) -> jsonschema::Validator {
    let yaml: serde_yaml::Value = serde_yaml::from_str(include_str!(
        "../../../../docs/public/api-reference/fabro-api.yaml"
    ))
    .unwrap();
    let mut spec = serde_json::to_value(yaml).unwrap();
    spec["$ref"] = json!(format!("#/components/schemas/{name}"));
    jsonschema::validator_for(&spec).unwrap()
}

#[test]
fn probe_api_reuses_library_types_and_preserves_every_outcome() {
    assert_eq!(
        TypeId::of::<types::ProbeReport>(),
        TypeId::of::<ProbeReport>()
    );
    assert_eq!(
        TypeId::of::<types::LlmErrorData>(),
        TypeId::of::<ErrorData>()
    );
    let error = json!({
        "kind": "authentication", "message": "invalid key", "provider": "acme",
        "status": 401, "provider_code": "invalid_key", "retry": {"type": "never"},
        "raw_data": {"error": "detail"}, "source_message": "source",
        "provider_retry_after_millis": 12
    });
    let parsed: types::LlmErrorData = serde_json::from_value(error.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), error);
    assert!(validator("LlmErrorData").is_valid(&error));

    let schema = validator("ProbeReport");
    for (route, outcome) in [
        (json!("acme/vendor/model"), json!({"status": "passed"})),
        (Value::Null, json!({"status": "failed", "error": error})),
        (
            json!("acme/model"),
            json!({"status": "incorrect", "detail": "no tool call"}),
        ),
    ] {
        let value = json!({
            "version": 1, "route": route, "outcome": outcome, "latency_ms": 123,
            "usage": TokenCounts { input: 12, output: 7, ..TokenCounts::default() }
        });
        let report: types::ProbeReport = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(report).unwrap(), value);
        assert!(schema.is_valid(&value), "{value}");
    }
}

#[test]
fn probe_schema_and_rust_reject_unsupported_versions_routes_and_outcomes() {
    let schema = validator("ProbeReport");
    let valid = json!({
        "version": 1, "route": "acme/model", "outcome": {"status": "passed"},
        "latency_ms": 0, "usage": TokenCounts::default()
    });
    for (field, invalid) in [
        ("version", json!(2)),
        ("route", json!("model")),
        ("route", json!("acme/")),
        ("outcome", json!({"status": "skip"})),
        ("outcome", json!({"status": "failed"})),
        ("latency_ms", json!(-1)),
    ] {
        let mut value = valid.clone();
        value[field] = invalid;
        assert!(!schema.is_valid(&value), "{value}");
        assert!(serde_json::from_value::<types::ProbeReport>(value).is_err());
    }
}
