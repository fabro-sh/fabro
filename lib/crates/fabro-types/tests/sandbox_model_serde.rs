use std::collections::BTreeMap;

use chrono::{TimeZone, Utc};
use fabro_types::{
    RunSandbox, RunSandboxInstance, RunSandboxPlan, RunSandboxRuntime, SandboxDetails,
    SandboxNetwork, SandboxProviderKind, SandboxResources, SandboxState, SandboxTimestamps,
};
use serde_json::json;

#[test]
fn run_sandbox_serializes_canonical_identity_without_identifier() {
    let sandbox = RunSandbox::ready(
        RunSandboxPlan {
            provider: SandboxProviderKind::Docker,
            image:    None,
            snapshot: None,
        },
        RunSandboxInstance {
            provider: SandboxProviderKind::Docker,
            image:    None,
            snapshot: None,
            runtime:  RunSandboxRuntime {
                id:                "container-abc123".to_string(),
                working_directory: "/workspace".to_string(),
                repo_cloned:       Some(true),
                clone_origin_url:  Some("https://github.com/fabro-sh/fabro.git".to_string()),
                clone_branch:      Some("main".to_string()),
                workspace_root:    Some("/workspace".to_string()),
                repos_root:        Some("/repos".to_string()),
                primary_repo_path: Some("/repos/fabro-sh/fabro".to_string()),
                primary_repo_link: Some("/workspace/fabro".to_string()),
            },
        },
    );

    let value = serde_json::to_value(&sandbox).unwrap();

    assert_eq!(
        value,
        json!({
            "kind": "ready",
            "plan": {
                "provider": "docker"
            },
            "instance": {
                "provider": "docker",
                "runtime": {
                    "id": "container-abc123",
                    "working_directory": "/workspace",
                    "repo_cloned": true,
                    "clone_origin_url": "https://github.com/fabro-sh/fabro.git",
                    "clone_branch": "main",
                    "workspace_root": "/workspace",
                    "repos_root": "/repos",
                    "primary_repo_path": "/repos/fabro-sh/fabro",
                    "primary_repo_link": "/workspace/fabro"
                }
            }
        })
    );
    assert!(value.get("identifier").is_none());
}

#[test]
fn run_sandbox_ready_requires_instance() {
    let sandbox = json!({
        "kind": "ready",
        "plan": { "provider": "docker" }
    });

    assert!(serde_json::from_value::<RunSandbox>(sandbox).is_err());
}

#[test]
fn sandbox_details_requires_canonical_id_and_working_directory() {
    let details = SandboxDetails {
        sandbox:      RunSandboxInstance {
            provider: SandboxProviderKind::Daytona,
            image:    Some("ubuntu:24.04".to_string()),
            snapshot: None,
            runtime:  RunSandboxRuntime {
                id:                "daytona-sandbox-name".to_string(),
                working_directory: "/workspace".to_string(),
                repo_cloned:       None,
                clone_origin_url:  None,
                clone_branch:      None,
                workspace_root:    Some("/home/daytona/workspace".to_string()),
                repos_root:        Some("/home/daytona/repos".to_string()),
                primary_repo_path: None,
                primary_repo_link: None,
            },
        },
        state:        SandboxState::Running,
        native_state: Some("started".to_string()),
        region:       Some("us".to_string()),
        web_url:      Some(
            "https://app.daytona.io/dashboard/sandboxes?sandboxId=ad65029a-2d01-421e-8936-49451653fcd9"
                .to_string(),
        ),
        resources:    SandboxResources {
            cpu_cores:    Some(2.0),
            memory_bytes: Some(4 * 1024 * 1024 * 1024),
            disk_bytes:   None,
        },
        network:      SandboxNetwork::unknown(),
        labels:       BTreeMap::from([("run".to_string(), "abc".to_string())]),
        timestamps:   SandboxTimestamps {
            created_at:       Some(Utc.with_ymd_and_hms(2026, 5, 9, 12, 0, 0).unwrap()),
            last_activity_at: None,
        },
    };

    let value = serde_json::to_value(&details).unwrap();

    assert_eq!(value["sandbox"]["provider"], "daytona");
    assert_eq!(value["sandbox"]["runtime"]["id"], "daytona-sandbox-name");
    assert_eq!(
        value["sandbox"]["runtime"]["working_directory"],
        "/workspace"
    );
    assert_eq!(
        value["sandbox"]["runtime"]["workspace_root"],
        "/home/daytona/workspace"
    );
    assert_eq!(
        value["sandbox"]["runtime"]["repos_root"],
        "/home/daytona/repos"
    );
    assert_eq!(
        value["web_url"],
        "https://app.daytona.io/dashboard/sandboxes?sandboxId=ad65029a-2d01-421e-8936-49451653fcd9"
    );
    assert_eq!(value["network"]["egress"]["mode"], "unknown");
    assert_eq!(value["network"]["ingress"]["mode"], "unknown");
    assert!(value.get("name").is_none());
    assert!(value.get("identifier").is_none());
}

#[test]
fn sandbox_provider_rejects_unknown_values() {
    assert_eq!(
        serde_json::from_value::<SandboxProviderKind>(json!("local")).unwrap(),
        SandboxProviderKind::Local
    );
    assert_eq!(
        serde_json::from_value::<SandboxProviderKind>(json!("docker")).unwrap(),
        SandboxProviderKind::Docker
    );
    assert_eq!(
        serde_json::from_value::<SandboxProviderKind>(json!("daytona")).unwrap(),
        SandboxProviderKind::Daytona
    );
    assert!(serde_json::from_value::<SandboxProviderKind>(json!("other")).is_err());
}
