use fabro_types::{
    RunSandbox, RunSandboxInstance, RunSandboxPlan, RunSandboxRuntime, SandboxDetails,
    SandboxProviderKind,
};
use sandbox_driver::{SandboxId, SandboxState, SandboxStatus};
use serde_json::json;

#[test]
fn run_sandbox_serializes_canonical_identity_without_identifier() {
    let sandbox = RunSandbox::ready(
        RunSandboxPlan {
            provider: SandboxProviderKind::DOCKER,
            image:    None,
            snapshot: None,
        },
        RunSandboxInstance {
            provider:          SandboxProviderKind::DOCKER,
            image:             None,
            snapshot:          None,
            runtime:           RunSandboxRuntime {
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
            ready_duration_ms: None,
            retained:          None,
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
fn sandbox_details_keep_the_record_beside_the_status() {
    let mut status = SandboxStatus::new(
        SandboxId::try_new("daytona-sandbox-name").unwrap(),
        SandboxState::Running,
    );
    status.provider_state = "started".to_string();
    status.region = Some("us".to_string());
    status.web_url = Some(
        "https://app.daytona.io/dashboard/sandboxes?sandboxId=ad65029a-2d01-421e-8936-49451653fcd9"
            .to_string(),
    );
    let details = SandboxDetails {
        sandbox: RunSandboxInstance {
            provider:          SandboxProviderKind::DAYTONA,
            image:             Some("ubuntu:24.04".to_string()),
            snapshot:          None,
            runtime:           RunSandboxRuntime {
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
            ready_duration_ms: None,
            retained:          None,
        },
        status,
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
        value["status"]["web_url"],
        "https://app.daytona.io/dashboard/sandboxes?sandboxId=ad65029a-2d01-421e-8936-49451653fcd9"
    );
    assert_eq!(value["status"]["provider_state"], "started");
    assert_eq!(value["status"]["network"], serde_json::Value::Null);
    assert!(value.get("identifier").is_none());
}

#[test]
fn sandbox_provider_accepts_plugin_kinds_and_rejects_malformed_names() {
    assert_eq!(
        serde_json::from_value::<SandboxProviderKind>(json!("local")).unwrap(),
        SandboxProviderKind::LOCAL
    );
    assert_eq!(
        serde_json::from_value::<SandboxProviderKind>(json!("docker")).unwrap(),
        SandboxProviderKind::DOCKER
    );
    assert_eq!(
        serde_json::from_value::<SandboxProviderKind>(json!("daytona")).unwrap(),
        SandboxProviderKind::DAYTONA
    );
    let plugin = serde_json::from_value::<SandboxProviderKind>(json!("other")).unwrap();
    assert_eq!(plugin.as_str(), "other");
    assert_eq!(plugin.bundled(), None);
    assert!(serde_json::from_value::<SandboxProviderKind>(json!("Not Valid")).is_err());
    assert!(serde_json::from_value::<SandboxProviderKind>(json!("")).is_err());
}
