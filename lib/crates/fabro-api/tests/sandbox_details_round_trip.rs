use std::any::{TypeId, type_name};
use std::collections::BTreeMap;

use chrono::{TimeZone, Utc};
use fabro_api::types::{
    SandboxDetails as ApiSandboxDetails, SandboxNetwork as ApiSandboxNetwork,
    SandboxNetworkPolicy as ApiSandboxNetworkPolicy,
    SandboxNetworkPolicyMode as ApiSandboxNetworkPolicyMode, SandboxProvider as ApiSandboxProvider,
    SandboxResources as ApiSandboxResources, SandboxState as ApiSandboxState,
    SandboxTimestamps as ApiSandboxTimestamps,
};
use fabro_types::{
    RunSandbox, RunSandboxRuntime, SandboxDetails, SandboxNetwork, SandboxNetworkPolicy,
    SandboxNetworkPolicyMode, SandboxProvider, SandboxResources, SandboxState, SandboxTimestamps,
};
use serde_json::json;

#[test]
fn sandbox_details_reuses_domain_types() {
    assert_same_type::<ApiSandboxDetails, SandboxDetails>();
    assert_same_type::<ApiSandboxProvider, SandboxProvider>();
    assert_same_type::<ApiSandboxState, SandboxState>();
    assert_same_type::<ApiSandboxResources, SandboxResources>();
    assert_same_type::<ApiSandboxTimestamps, SandboxTimestamps>();
    assert_same_type::<ApiSandboxNetwork, SandboxNetwork>();
    assert_same_type::<ApiSandboxNetworkPolicy, SandboxNetworkPolicy>();
    assert_same_type::<ApiSandboxNetworkPolicyMode, SandboxNetworkPolicyMode>();
}

#[test]
fn sandbox_details_json_matches_openapi_shape() {
    let created_at = Utc.with_ymd_and_hms(2026, 5, 9, 12, 0, 0).unwrap();
    let details = SandboxDetails {
        sandbox:      RunSandbox {
            provider: SandboxProvider::Docker,
            image:    Some("ghcr.io/fabro/sandbox:latest".to_string()),
            snapshot: None,
            runtime:  Some(RunSandboxRuntime {
                id:                "container-abc123".to_string(),
                working_directory: "/workspace".to_string(),
                repo_cloned:       None,
                clone_origin_url:  None,
                clone_branch:      None,
                workspace_root:    Some("/workspace".to_string()),
                repos_root:        Some("/repos".to_string()),
                primary_repo_path: Some("/repos/fabro-sh/fabro".to_string()),
                primary_repo_link: Some("/workspace/fabro".to_string()),
            }),
        },
        state:        SandboxState::Running,
        native_state: Some("running".to_string()),
        region:       None,
        web_url:      Some(
            "https://app.daytona.io/dashboard/sandboxes?sandboxId=ad65029a-2d01-421e-8936-49451653fcd9"
                .to_string(),
        ),
        resources:    SandboxResources {
            cpu_cores:    Some(2.0),
            memory_bytes: Some(4 * 1024 * 1024 * 1024),
            disk_bytes:   None,
        },
        network:      SandboxNetwork {
            egress:  SandboxNetworkPolicy::open(),
            ingress: SandboxNetworkPolicy::blocked(),
        },
        labels:       BTreeMap::from([("run".to_string(), "abc".to_string())]),
        timestamps:   SandboxTimestamps {
            created_at:       Some(created_at),
            last_activity_at: None,
        },
    };

    assert_eq!(
        serde_json::to_value(&details).unwrap(),
        json!({
            "sandbox": {
                "provider": "docker",
                "image": "ghcr.io/fabro/sandbox:latest",
                "runtime": {
                    "id": "container-abc123",
                    "working_directory": "/workspace",
                    "workspace_root": "/workspace",
                    "repos_root": "/repos",
                    "primary_repo_path": "/repos/fabro-sh/fabro",
                    "primary_repo_link": "/workspace/fabro"
                }
            },
            "state": "running",
            "native_state": "running",
            "web_url": "https://app.daytona.io/dashboard/sandboxes?sandboxId=ad65029a-2d01-421e-8936-49451653fcd9",
            "resources": {
                "cpu_cores": 2.0,
                "memory_bytes": 4_294_967_296_u64,
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
                "run": "abc"
            },
            "timestamps": {
                "created_at": "2026-05-09T12:00:00Z"
            }
        })
    );
}

#[test]
fn sandbox_details_deserializes_when_optional_fields_are_absent() {
    let details: SandboxDetails = serde_json::from_value(json!({
        "sandbox": {
            "provider": "local",
            "runtime": {
                "id": "local:01JNQVR7M0EJ5GKAT2SC4ERS1Z",
                "working_directory": "/Users/client/project"
            }
        },
        "state": "unknown",
        "resources": {},
        "labels": {},
        "timestamps": {}
    }))
    .unwrap();

    assert_eq!(details.sandbox.provider, SandboxProvider::Local);
    assert_eq!(
        details
            .sandbox
            .runtime
            .as_ref()
            .map(|runtime| runtime.id.as_str()),
        Some("local:01JNQVR7M0EJ5GKAT2SC4ERS1Z")
    );
    assert_eq!(
        details
            .sandbox
            .runtime
            .as_ref()
            .map(|runtime| runtime.working_directory.as_str()),
        Some("/Users/client/project")
    );
    assert_eq!(details.state, SandboxState::Unknown);
    assert!(details.sandbox.image.is_none());
    assert!(details.region.is_none());
    assert!(details.native_state.is_none());
    assert!(details.labels.is_empty());
    assert_eq!(details.resources, SandboxResources::default());
    assert_eq!(details.network, SandboxNetwork::unknown());
    assert_eq!(details.timestamps, SandboxTimestamps::default());
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
