use chrono::{DateTime, Utc};
use fabro_types::{
    SandboxInfo, SandboxListMeta, SandboxListResponse, SandboxNetworkPolicy, SandboxProviderKind,
    SandboxProviderLookupError, SandboxResources, SandboxState, SandboxStatus,
};
use serde_json::json;

#[test]
fn sandbox_inventory_serializes_the_provider_and_the_drivers_status() {
    let mut status = SandboxStatus::new("container-abc123", SandboxState::Running);
    status.name = Some("fabro-run-abc".to_string());
    status.provider_state = "running".to_string();
    status.image = Some("buildpack-deps:noble".to_string());
    status.resources = Some(SandboxResources {
        cpu_cores: Some(2),
        memory_mb: Some(4096),
        ..SandboxResources::default()
    });
    status.network = Some(SandboxNetworkPolicy::AllowAll);
    status
        .labels
        .insert("sh.fabro.managed".to_string(), "true".to_string());
    status.created_at = Some(
        DateTime::parse_from_rfc3339("2026-05-25T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    );
    let response = SandboxListResponse {
        data: vec![SandboxInfo {
            provider: SandboxProviderKind::DOCKER,
            status,
        }],
        meta: SandboxListMeta {
            provider_errors: vec![SandboxProviderLookupError {
                provider: SandboxProviderKind::DAYTONA,
                message:  "Daytona API key is not configured".to_string(),
            }],
        },
    };

    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({
            "data": [{
                "provider": "docker",
                "status": {
                    "id": "container-abc123",
                    "name": "fabro-run-abc",
                    "state": "running",
                    "provider_state": "running",
                    "error_reason": null,
                    "resources": {
                        "cpu_cores": 2,
                        "memory_mb": 4096,
                        "disk_mb": null,
                        "gpus": null
                    },
                    "sandbox_kind": null,
                    "region": null,
                    "labels": { "sh.fabro.managed": "true" },
                    "image": "buildpack-deps:noble",
                    "snapshot": null,
                    "network": "allow_all",
                    "workspace_ownership": null,
                    "web_url": null,
                    "created_at": "2026-05-25T12:00:00Z",
                    "updated_at": null
                }
            }],
            "meta": {
                "provider_errors": [{
                    "provider": "daytona",
                    "message": "Daytona API key is not configured"
                }]
            }
        })
    );
}

#[test]
fn sandbox_inventory_deserializes_a_status_with_only_its_required_fields() {
    let info: SandboxInfo = serde_json::from_value(json!({
        "provider": "local",
        "status": { "id": "host-dir-2f746d70", "state": "unknown" }
    }))
    .unwrap();

    assert_eq!(info.provider, SandboxProviderKind::LOCAL);
    assert_eq!(info.status.id.as_str(), "host-dir-2f746d70");
    assert_eq!(info.status.state, SandboxState::Unknown);
    assert!(info.status.name.is_none());
    assert!(info.status.resources.is_none());
    assert!(info.status.network.is_none());
    assert!(info.status.labels.is_empty());
}
