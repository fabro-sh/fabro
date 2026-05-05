//! JSON parity test for `RunIntegrationsGithubSettings`.
//!
//! Asserts that the API-side generated `RunIntegrationsGithubSettings` and
//! the canonical Rust resolved type round-trip through the same JSON shape.
//! Covers both the populated and empty-permissions cases.

use fabro_api::types::{
    RunIntegrationsGithubSettings as ApiRunIntegrationsGithubSettings,
    RunIntegrationsSettings as ApiRunIntegrationsSettings,
};
use fabro_types::settings::run::{RunIntegrationsGithubSettings, RunIntegrationsSettings};
use serde_json::json;

#[test]
fn run_integrations_github_settings_round_trips_with_permissions() {
    let json_value = json!({
        "permissions": {
            "issues": "read",
            "contents": "write",
        }
    });

    let api: ApiRunIntegrationsGithubSettings =
        serde_json::from_value(json_value.clone()).expect("api type should parse");
    let canonical: RunIntegrationsGithubSettings =
        serde_json::from_value(json_value.clone()).expect("canonical type should parse");

    assert_eq!(serde_json::to_value(&api).unwrap(), json_value);
    assert_eq!(serde_json::to_value(&canonical).unwrap(), json_value);
}

#[test]
fn run_integrations_github_settings_round_trips_empty_permissions() {
    // Empty map is the resolved form of "no token requested" — must
    // serialize as an object, not omitted.
    let json_value = json!({ "permissions": {} });

    let api: ApiRunIntegrationsGithubSettings =
        serde_json::from_value(json_value.clone()).expect("api type should parse empty");
    let canonical: RunIntegrationsGithubSettings =
        serde_json::from_value(json_value.clone()).expect("canonical type should parse empty");

    assert_eq!(serde_json::to_value(&api).unwrap(), json_value);
    assert_eq!(serde_json::to_value(&canonical).unwrap(), json_value);
}

#[test]
fn run_integrations_settings_round_trips() {
    let json_value = json!({
        "github": {
            "permissions": {
                "issues": "read",
            }
        }
    });

    let api: ApiRunIntegrationsSettings =
        serde_json::from_value(json_value.clone()).expect("api wrapper should parse");
    let canonical: RunIntegrationsSettings =
        serde_json::from_value(json_value.clone()).expect("canonical wrapper should parse");

    assert_eq!(serde_json::to_value(&api).unwrap(), json_value);
    assert_eq!(serde_json::to_value(&canonical).unwrap(), json_value);
}
