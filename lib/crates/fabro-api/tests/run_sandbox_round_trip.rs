use std::any::{TypeId, type_name};

use fabro_api::types::{RunSandbox as ApiRunSandbox, SandboxProviderKind as ApiSandboxProvider};
use fabro_types::{RunSandbox, RunSandboxRuntime, SandboxProviderKind};
use serde_json::json;

#[test]
fn run_sandbox_reuses_domain_types() {
    assert_same_type::<ApiRunSandbox, RunSandbox>();
    assert_same_type::<ApiSandboxProvider, SandboxProviderKind>();
}

#[test]
fn run_sandbox_json_matches_openapi_shape() {
    let sandbox = RunSandbox {
        provider: SandboxProviderKind::Docker,
        image:    Some("ghcr.io/fabro/sandbox:latest".to_string()),
        snapshot: None,
        runtime:  Some(RunSandboxRuntime {
            id:                "container-abc123".to_string(),
            working_directory: "/workspace".to_string(),
            repo_cloned:       Some(false),
            clone_origin_url:  Some("https://github.com/fabro-sh/fabro.git".to_string()),
            clone_branch:      Some("main".to_string()),
            workspace_root:    Some("/workspace".to_string()),
            repos_root:        Some("/repos".to_string()),
            primary_repo_path: None,
            primary_repo_link: None,
        }),
    };

    let value = serde_json::to_value(&sandbox).unwrap();

    assert_eq!(
        value,
        json!({
            "provider": "docker",
            "image": "ghcr.io/fabro/sandbox:latest",
            "runtime": {
                "id": "container-abc123",
                "working_directory": "/workspace",
                "repo_cloned": false,
                "clone_origin_url": "https://github.com/fabro-sh/fabro.git",
                "clone_branch": "main",
                "workspace_root": "/workspace",
                "repos_root": "/repos"
            }
        })
    );
    assert!(value.get("identifier").is_none());
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
