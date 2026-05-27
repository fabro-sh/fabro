use std::any::{TypeId, type_name};

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

#[test]
fn automation_contract_reuses_domain_types() {
    assert_same_type::<ApiAutomation, Automation>();
    assert_same_type::<ApiAutomationTarget, AutomationTarget>();
    assert_same_type::<ApiAutomationTrigger, AutomationTrigger>();
    assert_same_type::<ApiCreateAutomationRequest, AutomationDraft>();
    assert_same_type::<ApiReplaceAutomationRequest, AutomationReplace>();
}

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
    let domain: Automation = serde_json::from_value(value.clone()).unwrap();

    assert_eq!(api, domain);
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}

#[test]
fn create_automation_request_round_trips_public_json_shape() {
    let value = json!({
        "id": "nightly-deps",
        "name": "Nightly dependency update",
        "description": "Keep dependencies fresh",
        "enabled": false,
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
    let domain: AutomationDraft = serde_json::from_value(value.clone()).unwrap();

    assert_eq!(api, domain);
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
    let domain: AutomationReplace = serde_json::from_value(value.clone()).unwrap();

    assert_eq!(api, domain);
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}

fn assert_same_type<Api: 'static, Domain: 'static>() {
    assert_eq!(
        TypeId::of::<Api>(),
        TypeId::of::<Domain>(),
        "{} should be reused as {}",
        type_name::<Api>(),
        type_name::<Domain>(),
    );
}
