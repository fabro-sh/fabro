//! Every usage on the API is lithos-llm's `Usage`: tokens with an optional
//! priced cost. These tests prove the API type is lithos-llm's own, and that
//! the `Usage` and `ModelUsage` schemas describe its serde shape, including
//! the absent `cost`.

use std::any::{TypeId, type_name};

use fabro_api::types::{ModelUsage as ApiModelUsage, Usage as ApiUsage};
use fabro_types::{ModelRef, ModelUsage};
use lithos_llm::catalog::{ModelId, ProviderId};
use lithos_llm::types::{Cost, CostSource, Speed, TokenCounts, Usage};
use serde_json::{Value, json};

#[test]
fn usage_types_reuse_canonical_types() {
    assert_same_type::<ApiUsage, Usage>();
    assert_same_type::<ApiModelUsage, ModelUsage>();
}

#[test]
fn usage_json_matches_openapi_shape() {
    let usage = Usage {
        tokens: TokenCounts {
            input:            10,
            output:           20,
            reasoning:        3,
            cache_read:       1,
            cache_write:      1,
            long_cache_write: 0,
        },
        cost:   Some(Cost {
            usd_micros: 42,
            source:     CostSource::Catalog,
        }),
    };

    let json = serde_json::to_value(usage).unwrap();
    assert_eq!(
        json,
        json!({
            "tokens": {
                "input": 10,
                "output": 20,
                "reasoning": 3,
                "cache_read": 1,
                "cache_write": 1
            },
            "cost": { "usd_micros": 42, "source": "catalog" }
        })
    );
    assert_valid("Usage", &json);

    let round_trip: ApiUsage = serde_json::from_value(json).unwrap();
    assert_eq!(round_trip, usage);
}

#[test]
fn usage_omits_an_absent_cost_and_reads_absent_buckets_as_zero() {
    let json = serde_json::to_value(Usage::default()).unwrap();
    assert_eq!(json.get("cost"), None);
    assert_eq!(json["tokens"]["reasoning"], 0);
    assert_valid("Usage", &json);

    let round_trip: ApiUsage = serde_json::from_value(json!({"tokens": {"input": 7}})).unwrap();
    assert_eq!(round_trip, Usage {
        tokens: TokenCounts {
            input: 7,
            ..TokenCounts::default()
        },
        cost:   None,
    });
}

#[test]
fn model_usage_json_matches_openapi_shape() {
    let usage = ModelUsage::new(
        ModelRef::new(
            ProviderId::new("anthropic"),
            ModelId::new("claude-sonnet-5"),
        )
        .with_speed(Some(Speed::Fast)),
        Usage {
            tokens: TokenCounts {
                input: 100,
                output: 20,
                ..TokenCounts::default()
            },
            cost:   Some(Cost {
                usd_micros: 720_000,
                source:     CostSource::Provider,
            }),
        },
    );

    let json = serde_json::to_value(&usage).unwrap();
    assert_eq!(
        json,
        json!({
            "model": {
                "provider": "anthropic",
                "model_id": "claude-sonnet-5",
                "speed": "fast"
            },
            "usage": {
                "tokens": {
                    "input": 100,
                    "output": 20,
                    "reasoning": 0,
                    "cache_read": 0,
                    "cache_write": 0
                },
                "cost": { "usd_micros": 720000, "source": "provider" }
            }
        })
    );
    assert_valid("ModelUsage", &json);

    let round_trip: ApiModelUsage = serde_json::from_value(json).unwrap();
    assert_eq!(round_trip, usage);
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

/// Validates `value` against one component schema, with the whole document
/// as the root so `$ref`s resolve.
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
