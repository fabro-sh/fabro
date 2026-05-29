use fabro_api::types::{
    CreateEnvironmentRequest as ApiCreateEnvironmentRequest, Environment as ApiEnvironment,
    ReplaceEnvironmentRequest as ApiReplaceEnvironmentRequest,
};
use fabro_environment::Environment;
use serde_json::json;

// Compile-time witness that the generated API response type resolves to the
// same type as the `fabro-environment` domain type via `with_replacement(...)`.
// Request types intentionally stay API-specific so REST Dockerfile sources can
// remain inline-only without changing workflow/settings schemas.
const _: fn(ApiEnvironment) -> Environment = |value| value;

fn environment_settings_json() -> serde_json::Value {
    json!({
        "provider": "docker",
        "image": {
            "docker": null,
            "dockerfile": {
                "type": "inline",
                "value": "FROM alpine\n"
            }
        },
        "resources": {
            "cpu": null,
            "memory": null,
            "disk": null
        },
        "network": {
            "mode": "allow_all",
            "allow": []
        },
        "lifecycle": {
            "preserve": false,
            "stop_on_terminal": true,
            "auto_stop": null
        },
        "labels": {},
        "volumes": [],
        "env": {}
    })
}

#[test]
fn environment_response_round_trips_public_json_shape() {
    let mut value = environment_settings_json();
    value["id"] = json!("docker-inline");
    value["revision"] = json!("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");

    let api: ApiEnvironment = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}

#[test]
fn create_environment_request_round_trips_inline_dockerfile_json_shape() {
    let mut value = environment_settings_json();
    value["id"] = json!("docker-inline");

    let api: ApiCreateEnvironmentRequest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}

#[test]
fn replace_environment_request_round_trips_inline_dockerfile_json_shape() {
    let value = environment_settings_json();

    let api: ApiReplaceEnvironmentRequest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}

#[test]
fn environment_request_schema_rejects_dockerfile_path_sources() {
    let mut value = environment_settings_json();
    value["id"] = json!("docker-path");
    value["image"]["dockerfile"] = json!({
        "type": "path",
        "path": "Dockerfile"
    });

    let err = serde_json::from_value::<ApiCreateEnvironmentRequest>(value)
        .expect_err("generated REST request type should reject Dockerfile path sources");
    assert!(
        err.to_string().contains("dockerfile")
            || err.to_string().contains("type")
            || err.to_string().contains("path"),
        "unexpected error: {err}"
    );
}
