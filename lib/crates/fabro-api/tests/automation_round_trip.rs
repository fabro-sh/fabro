use fabro_api::types::{
    Automation as ApiAutomation, AutomationTarget as ApiAutomationTarget,
    AutomationTrigger as ApiAutomationTrigger,
    CreateAutomationRequest as ApiCreateAutomationRequest,
    ReplaceAutomationRequest as ApiReplaceAutomationRequest,
};
use fabro_automation::{
    Automation, AutomationDraft, AutomationReplace, AutomationTarget, AutomationTrigger,
};
use serde_json::json;

// Compile-time witnesses that the generated API types resolve to the same
// types as the `fabro-automation` domain types via `with_replacement(...)`.
// If progenitor stops reusing the domain type, these functions stop type-
// checking and the build fails.
const _: fn(ApiAutomation) -> Automation = |value| value;
const _: fn(ApiAutomationTarget) -> AutomationTarget = |value| value;
const _: fn(ApiAutomationTrigger) -> AutomationTrigger = |value| value;
const _: fn(ApiCreateAutomationRequest) -> AutomationDraft = |value| value;
const _: fn(ApiReplaceAutomationRequest) -> AutomationReplace = |value| value;

#[test]
fn automation_response_round_trips_public_json_shape() {
    let value = json!({
        "id": "nightly-deps",
        "revision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "name": "Nightly dependency update",
        "description": null,
        "enabled": true,
        "target": {
            "repository": "fabro-sh/fabro",
            "ref": "main",
            "workflow": "dependency-update"
        },
        "triggers": [
            {
                "id": "manual",
                "type": "api",
                "enabled": true
            },
            {
                "id": "nightly",
                "type": "schedule",
                "enabled": true,
                "expression": "0 3 * * *"
            }
        ]
    });

    let api: ApiAutomation = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}

#[test]
fn create_automation_request_round_trips_public_json_shape() {
    let value = json!({
        "id": "nightly-deps",
        "name": "Nightly dependency update",
        "description": "Keep dependencies fresh",
        "enabled": true,
        "target": {
            "repository": "fabro-sh/fabro",
            "ref": "main",
            "workflow": "dependency-update"
        },
        "triggers": [
            {
                "id": "manual",
                "type": "api",
                "enabled": false
            }
        ]
    });

    let api: ApiCreateAutomationRequest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}

#[test]
fn replace_automation_request_round_trips_public_json_shape() {
    let value = json!({
        "name": "Nightly dependency update",
        "description": "Keep dependencies fresh",
        "enabled": true,
        "target": {
            "repository": "fabro-sh/fabro",
            "ref": "main",
            "workflow": "dependency-update"
        },
        "triggers": [
            {
                "id": "nightly",
                "type": "schedule",
                "enabled": true,
                "expression": "0 3 * * *"
            }
        ]
    });

    let api: ApiReplaceAutomationRequest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}
