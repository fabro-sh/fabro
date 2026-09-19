use std::any::{TypeId, type_name};
use std::time::SystemTime;

use chrono::DateTime;
use fabro_api::types::{
    SandboxDetails as ApiSandboxDetails, SandboxId as ApiSandboxId, SandboxKind as ApiSandboxKind,
    SandboxNetworkPolicy as ApiSandboxNetworkPolicy, SandboxProviderKind as ApiSandboxProvider,
    SandboxResources as ApiSandboxResources, SandboxState as ApiSandboxState,
    SandboxStatus as ApiSandboxStatus, SandboxWorkspaceOwnership as ApiSandboxWorkspaceOwnership,
};
use fabro_types::{RunSandboxInstance, RunSandboxRuntime, SandboxDetails, SandboxProviderKind};
use sandbox_driver::{
    NetworkPolicy, Resources, SandboxId, SandboxKind, SandboxState, SandboxStatus,
    WorkspaceOwnership,
};
use serde_json::json;

#[test]
fn sandbox_details_reuses_the_domain_and_driver_types() {
    assert_same_type::<ApiSandboxDetails, SandboxDetails>();
    assert_same_type::<ApiSandboxProvider, SandboxProviderKind>();
    assert_same_type::<ApiSandboxStatus, SandboxStatus>();
    assert_same_type::<ApiSandboxId, SandboxId>();
    assert_same_type::<ApiSandboxState, SandboxState>();
    assert_same_type::<ApiSandboxResources, Resources>();
    assert_same_type::<ApiSandboxNetworkPolicy, NetworkPolicy>();
    assert_same_type::<ApiSandboxKind, SandboxKind>();
    assert_same_type::<ApiSandboxWorkspaceOwnership, WorkspaceOwnership>();
}

#[test]
fn sandbox_details_json_matches_openapi_shape() {
    let mut status = SandboxStatus::new(
        SandboxId::try_new("container-abc123").unwrap(),
        SandboxState::Running,
    );
    status.name = Some("fabro-run-abc".to_string());
    status.provider_state = "running".to_string();
    let mut resources = Resources::default();
    resources.cpu_cores = Some(2);
    resources.memory_mb = Some(4096);
    status.resources = Some(resources);
    status.sandbox_kind = Some(SandboxKind::Container);
    status.labels.insert("run".to_string(), "abc".to_string());
    status.image = Some("ghcr.io/fabro/sandbox:latest".to_string());
    status.network = Some(NetworkPolicy::CidrAllowList {
        cidrs: vec!["10.0.0.0/8".to_string()],
    });
    status.web_url = Some(
        "https://app.daytona.io/dashboard/sandboxes?sandboxId=ad65029a-2d01-421e-8936-49451653fcd9"
            .to_string(),
    );
    status.created_at = Some(SystemTime::from(
        DateTime::parse_from_rfc3339("2026-05-09T12:00:00Z").unwrap(),
    ));
    let details = SandboxDetails {
        sandbox: RunSandboxInstance {
            provider:          SandboxProviderKind::DOCKER,
            image:             Some("ghcr.io/fabro/sandbox:latest".to_string()),
            snapshot:          None,
            runtime:           RunSandboxRuntime {
                id:                "container-abc123".to_string(),
                working_directory: "/workspace".to_string(),
                repo_cloned:       None,
                clone_origin_url:  None,
                clone_branch:      None,
                workspace_root:    Some("/workspace".to_string()),
                repos_root:        Some("/repos".to_string()),
                primary_repo_path: Some("/repos/fabro-sh/fabro".to_string()),
                primary_repo_link: Some("/workspace/fabro".to_string()),
            },
            ready_duration_ms: None,
            retained:          None,
        },
        status,
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
                "sandbox_kind": "container",
                "region": null,
                "labels": { "run": "abc" },
                "image": "ghcr.io/fabro/sandbox:latest",
                "snapshot": null,
                "network": { "cidr_allow_list": { "cidrs": ["10.0.0.0/8"] } },
                "workspace_ownership": null,
                "web_url": "https://app.daytona.io/dashboard/sandboxes?sandboxId=ad65029a-2d01-421e-8936-49451653fcd9",
                "created_at": "2026-05-09T12:00:00Z",
                "updated_at": null
            }
        })
    );
}

#[test]
fn sandbox_details_deserializes_a_status_with_only_its_required_fields() {
    let details: SandboxDetails = serde_json::from_value(json!({
        "sandbox": {
            "provider": "local",
            "runtime": {
                "id": "host-dir-2f55736572732f636c69656e742f70726f6a656374",
                "working_directory": "/Users/client/project"
            }
        },
        "status": {
            "id": "host-dir-2f55736572732f636c69656e742f70726f6a656374",
            "state": "running",
            "workspace_ownership": "designated"
        }
    }))
    .unwrap();

    assert_eq!(details.sandbox.provider, SandboxProviderKind::LOCAL);
    assert_eq!(details.status.state, SandboxState::Running);
    assert_eq!(
        details.status.workspace_ownership,
        Some(WorkspaceOwnership::Designated)
    );
    assert!(details.status.resources.is_none());
    assert!(details.status.network.is_none());
    assert!(details.status.created_at.is_none());
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
