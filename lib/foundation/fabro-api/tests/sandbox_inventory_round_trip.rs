use std::any::{TypeId, type_name};
use std::time::SystemTime;

use chrono::DateTime;
use fabro_api::types::{
    SandboxInfo as ApiSandboxInfo, SandboxListMeta as ApiSandboxListMeta,
    SandboxListResponse as ApiSandboxListResponse, SandboxProviderKind as ApiSandboxProviderKind,
    SandboxProviderLookupError as ApiSandboxProviderLookupError,
};
use fabro_types::{
    SandboxInfo, SandboxListMeta, SandboxListResponse, SandboxProviderKind,
    SandboxProviderLookupError,
};
use sandbox_driver::legacy::{NetworkPolicy, Resources, SandboxId, SandboxState, SandboxStatus};
use serde_json::json;

#[test]
fn sandbox_inventory_round_trip_reuses_domain_types() {
    assert_same_type::<ApiSandboxProviderKind, SandboxProviderKind>();
    assert_same_type::<ApiSandboxInfo, SandboxInfo>();
    assert_same_type::<ApiSandboxProviderLookupError, SandboxProviderLookupError>();
    assert_same_type::<ApiSandboxListMeta, SandboxListMeta>();
    assert_same_type::<ApiSandboxListResponse, SandboxListResponse>();
}

#[test]
fn sandbox_inventory_round_trip_json_matches_openapi_shape() {
    let mut status = SandboxStatus::new(
        SandboxId::try_new("sandbox-abc123").unwrap(),
        SandboxState::Running,
    );
    status.name = Some("fabro-01KSGHGMCFM8W2FHXNMJ7MVY65".to_string());
    status.provider_state = "started".to_string();
    let mut resources = Resources::default();
    resources.cpu_cores = Some(2);
    resources.memory_mb = Some(4096);
    resources.disk_mb = Some(20 * 1024);
    status.resources = Some(resources);
    status.region = Some("us".to_string());
    status
        .labels
        .insert("sh.fabro.managed".to_string(), "true".to_string());
    status.snapshot = Some("daytona-medium".to_string());
    status.network = Some(NetworkPolicy::Block);
    status.web_url =
        Some("https://app.daytona.io/dashboard/sandboxes?sandboxId=sandbox-abc123".to_string());
    let at = SystemTime::from(DateTime::parse_from_rfc3339("2026-05-25T12:00:00Z").unwrap());
    status.created_at = Some(at);
    status.updated_at = Some(at);
    let response = SandboxListResponse {
        data: vec![SandboxInfo {
            provider: SandboxProviderKind::DAYTONA,
            status,
        }],
        meta: SandboxListMeta {
            provider_errors: vec![SandboxProviderLookupError {
                provider: SandboxProviderKind::DOCKER,
                message:  "docker daemon unreachable".to_string(),
            }],
        },
    };

    let value = serde_json::to_value(&response).unwrap();
    assert_eq!(
        value,
        json!({
            "data": [{
                "provider": "daytona",
                "status": {
                    "id": "sandbox-abc123",
                    "name": "fabro-01KSGHGMCFM8W2FHXNMJ7MVY65",
                    "state": "running",
                    "provider_state": "started",
                    "error_reason": null,
                    "resources": {
                        "cpu_cores": 2,
                        "memory_mb": 4096,
                        "disk_mb": 20480,
                        "gpus": null
                    },
                    "sandbox_kind": null,
                    "region": "us",
                    "labels": { "sh.fabro.managed": "true" },
                    "image": null,
                    "snapshot": "daytona-medium",
                    "network": "block",
                    "workspace_ownership": null,
                    "web_url": "https://app.daytona.io/dashboard/sandboxes?sandboxId=sandbox-abc123",
                    "created_at": "2026-05-25T12:00:00Z",
                    "updated_at": "2026-05-25T12:00:00Z"
                }
            }],
            "meta": {
                "provider_errors": [{
                    "provider": "docker",
                    "message": "docker daemon unreachable"
                }]
            }
        })
    );

    let decoded: ApiSandboxListResponse = serde_json::from_value(value).unwrap();
    assert_eq!(decoded.data[0].provider, SandboxProviderKind::DAYTONA);
    assert_eq!(decoded.data[0].status.state, SandboxState::Running);
    assert!(matches!(
        decoded.data[0].status.network,
        Some(NetworkPolicy::Block)
    ));
}

fn assert_same_type<A: 'static, B: 'static>() {
    assert_eq!(
        TypeId::of::<A>(),
        TypeId::of::<B>(),
        "{} should be {}",
        type_name::<A>(),
        type_name::<B>()
    );
}
