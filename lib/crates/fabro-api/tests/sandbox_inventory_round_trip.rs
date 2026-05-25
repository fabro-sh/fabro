use std::any::{TypeId, type_name};
use std::collections::BTreeMap;

use chrono::{TimeZone, Utc};
use fabro_api::types::{
    SandboxInfo as ApiSandboxInfo, SandboxListMeta as ApiSandboxListMeta,
    SandboxListResponse as ApiSandboxListResponse, SandboxProviderKind as ApiSandboxProviderKind,
    SandboxProviderLookupError as ApiSandboxProviderLookupError,
};
use fabro_types::{
    SandboxInfo, SandboxListMeta, SandboxListResponse, SandboxNetwork, SandboxNetworkPolicy,
    SandboxProviderKind, SandboxProviderLookupError, SandboxResources, SandboxState,
    SandboxTimestamps,
};
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
    let created_at = Utc.with_ymd_and_hms(2026, 5, 25, 12, 0, 0).unwrap();
    let response = SandboxListResponse {
        data: vec![SandboxInfo {
            provider:          SandboxProviderKind::Daytona,
            id:                "sandbox-abc123".to_string(),
            display_name:      Some("fabro-01KSGHGMCFM8W2FHXNMJ7MVY65".to_string()),
            state:             SandboxState::Running,
            native_state:      Some("started".to_string()),
            image:             None,
            snapshot:          Some("daytona-medium".to_string()),
            region:            Some("us".to_string()),
            web_url:           Some(
                "https://app.daytona.io/dashboard/sandboxes?sandboxId=sandbox-abc123".to_string(),
            ),
            working_directory: Some("/home/daytona/workspace".to_string()),
            resources:         SandboxResources {
                cpu_cores:    Some(2.0),
                memory_bytes: Some(4 * 1024 * 1024 * 1024),
                disk_bytes:   Some(20 * 1024 * 1024 * 1024),
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
                last_activity_at: Some(created_at),
            },
        }],
        meta: SandboxListMeta {
            provider_errors: vec![SandboxProviderLookupError {
                provider: SandboxProviderKind::Docker,
                message:  "Failed to connect to Docker daemon".to_string(),
            }],
        },
    };

    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({
            "data": [{
                "provider": "daytona",
                "id": "sandbox-abc123",
                "display_name": "fabro-01KSGHGMCFM8W2FHXNMJ7MVY65",
                "state": "running",
                "native_state": "started",
                "snapshot": "daytona-medium",
                "region": "us",
                "web_url": "https://app.daytona.io/dashboard/sandboxes?sandboxId=sandbox-abc123",
                "working_directory": "/home/daytona/workspace",
                "resources": {
                    "cpu_cores": 2.0,
                    "memory_bytes": 4_294_967_296_u64,
                    "disk_bytes": 21_474_836_480_u64
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
                    "created_at": "2026-05-25T12:00:00Z",
                    "last_activity_at": "2026-05-25T12:00:00Z"
                }
            }],
            "meta": {
                "provider_errors": [{
                    "provider": "docker",
                    "message": "Failed to connect to Docker daemon"
                }]
            }
        })
    );
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
