use std::collections::BTreeMap;

use chrono::{TimeZone, Utc};
use fabro_types::{
    SandboxInfo, SandboxListMeta, SandboxListResponse, SandboxNetwork, SandboxNetworkPolicy,
    SandboxProviderKind, SandboxProviderLookupError, SandboxResources, SandboxState,
    SandboxTimestamps,
};
use serde_json::json;

#[test]
fn sandbox_inventory_serializes_provider_backed_shape() {
    let created_at = Utc.with_ymd_and_hms(2026, 5, 25, 12, 0, 0).unwrap();
    let response = SandboxListResponse {
        data: vec![SandboxInfo {
            provider:          SandboxProviderKind::Docker,
            id:                "container-abc123".to_string(),
            display_name:      Some("fabro-run-abc".to_string()),
            state:             SandboxState::Running,
            native_state:      Some("running".to_string()),
            image:             Some("buildpack-deps:noble".to_string()),
            snapshot:          None,
            region:            None,
            web_url:           None,
            working_directory: Some("/workspace".to_string()),
            resources:         SandboxResources {
                cpu_cores:    Some(2.0),
                memory_bytes: Some(4 * 1024 * 1024 * 1024),
                disk_bytes:   None,
            },
            network:           SandboxNetwork {
                egress:  SandboxNetworkPolicy::open(),
                ingress: SandboxNetworkPolicy::blocked(),
            },
            labels:            BTreeMap::from([(
                "sh.fabro.managed".to_string(),
                "true".to_string(),
            )]),
            timestamps:        SandboxTimestamps {
                created_at:       Some(created_at),
                last_activity_at: None,
            },
        }],
        meta: SandboxListMeta {
            provider_errors: vec![SandboxProviderLookupError {
                provider: SandboxProviderKind::Daytona,
                message:  "Daytona API key is not configured".to_string(),
            }],
        },
    };

    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({
            "data": [{
                "provider": "docker",
                "id": "container-abc123",
                "display_name": "fabro-run-abc",
                "state": "running",
                "native_state": "running",
                "image": "buildpack-deps:noble",
                "working_directory": "/workspace",
                "resources": {
                    "cpu_cores": 2.0,
                    "memory_bytes": 4_294_967_296_u64
                },
                "network": {
                    "egress": {
                        "mode": "open",
                        "cidrs": []
                    },
                    "ingress": {
                        "mode": "blocked",
                        "cidrs": []
                    }
                },
                "labels": {
                    "sh.fabro.managed": "true"
                },
                "timestamps": {
                    "created_at": "2026-05-25T12:00:00Z"
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
fn sandbox_inventory_deserializes_when_optional_fields_are_absent() {
    let info: SandboxInfo = serde_json::from_value(json!({
        "provider": "local",
        "id": "local:01KSGHGMCFM8W2FHXNMJ7MVY65",
        "state": "unknown",
        "resources": {},
        "timestamps": {}
    }))
    .unwrap();

    assert_eq!(info.provider, SandboxProviderKind::Local);
    assert_eq!(info.id, "local:01KSGHGMCFM8W2FHXNMJ7MVY65");
    assert_eq!(info.state, SandboxState::Unknown);
    assert!(info.display_name.is_none());
    assert!(info.native_state.is_none());
    assert!(info.image.is_none());
    assert!(info.snapshot.is_none());
    assert!(info.region.is_none());
    assert!(info.web_url.is_none());
    assert!(info.working_directory.is_none());
    assert_eq!(info.resources, SandboxResources::default());
    assert_eq!(info.network, SandboxNetwork::unknown());
    assert!(info.labels.is_empty());
    assert_eq!(info.timestamps, SandboxTimestamps::default());
}
