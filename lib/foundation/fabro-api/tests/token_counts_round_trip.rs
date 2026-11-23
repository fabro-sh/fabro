use std::any::{TypeId, type_name};

use fabro_api::types::TokenCounts as ApiTokenCounts;
use lithos_llm::types::TokenCounts;
use serde_json::{Value, json};

#[test]
fn token_counts_reuses_canonical_type() {
    assert_same_type::<ApiTokenCounts, TokenCounts>();
}

#[test]
fn token_counts_json_matches_openapi_shape() {
    let tokens = TokenCounts {
        input:            10,
        output:           20,
        reasoning:        3,
        cache_read:       4,
        cache_write:      5,
        long_cache_write: 0,
    };

    let json = serde_json::to_value(tokens).unwrap();
    assert_eq!(
        json,
        json!({
            "input": 10,
            "output": 20,
            "reasoning": 3,
            "cache_read": 4,
            "cache_write": 5
        })
    );
    assert_valid("TokenCounts", &json);

    let round_trip: ApiTokenCounts = serde_json::from_value(json).unwrap();
    assert_eq!(round_trip, tokens);
}

#[test]
fn token_counts_preserve_long_cache_writes_without_double_counting() {
    let tokens = TokenCounts {
        input: 10,
        cache_write: 5,
        long_cache_write: 3,
        ..TokenCounts::default()
    };
    let value = serde_json::to_value(tokens).unwrap();
    assert_eq!(value["long_cache_write"], json!(3));
    assert_valid("TokenCounts", &value);
    let round_trip: ApiTokenCounts = serde_json::from_value(value).unwrap();
    assert_eq!(round_trip, tokens);
    assert_eq!(round_trip.total(), 15);
}

#[test]
fn token_counts_missing_buckets_default_to_zero() {
    let round_trip: ApiTokenCounts = serde_json::from_value(json!({"input": 7})).unwrap();
    assert_eq!(round_trip, TokenCounts {
        input: 7,
        ..TokenCounts::default()
    });
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

#[expect(
    clippy::disallowed_methods,
    reason = "a synchronous test reads the spec from the repository once"
)]
fn spec() -> Value {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../docs/public/api-reference/fabro-api.yaml"
    ))
    .expect("the OpenAPI spec is in the repository");
    let yaml: serde_yaml::Value = serde_yaml::from_str(&text).expect("the spec parses");
    serde_json::to_value(yaml).expect("the spec is JSON-compatible")
}

fn assert_valid(schema_name: &str, value: &Value) {
    let mut root = spec();
    root["$ref"] = json!(format!("#/components/schemas/{schema_name}"));
    let validator = jsonschema::validator_for(&root).expect("the spec compiles as a JSON schema");
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|error| format!("{error} at {}", error.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{schema_name} rejects {value:#}:\n{}",
        errors.join("\n")
    );
}
