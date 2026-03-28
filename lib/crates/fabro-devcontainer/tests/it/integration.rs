use super::helpers::fixture_path;
use fabro_devcontainer::{Command, DevcontainerResolver};

#[tokio::test]
async fn resolve_image_only() {
    let config = DevcontainerResolver::resolve(&fixture_path("image-only"))
        .await
        .unwrap();

    assert!(
        config
            .dockerfile
            .contains("FROM mcr.microsoft.com/devcontainers/base:ubuntu")
    );
    assert_eq!(config.remote_user.as_deref(), Some("vscode"));
    assert_eq!(config.forwarded_ports, vec![3000, 80, 9090]);
    assert_eq!(
        config.environment.get("EDITOR").map(String::as_str),
        Some("code")
    );
    assert_eq!(config.workspace_folder, "/workspaces/image-only");
    assert!(config.compose_files.is_empty());
    assert!(config.compose_service.is_none());

    assert_eq!(config.post_create_commands.len(), 1);
    assert!(matches!(&config.post_create_commands[0], Command::Shell(s) if s == "echo hello"));

    // onCreateCommand
    assert_eq!(config.on_create_commands.len(), 1);
    assert!(matches!(&config.on_create_commands[0], Command::Shell(s) if s == "setup.sh"));

    // containerEnv baked into Dockerfile
    assert!(
        config
            .dockerfile
            .contains("ENV DEBIAN_FRONTEND=noninteractive")
    );
    assert_eq!(
        config
            .container_env
            .get("DEBIAN_FRONTEND")
            .map(String::as_str),
        Some("noninteractive")
    );
}

#[tokio::test]
async fn resolve_dockerfile_mode() {
    let config = DevcontainerResolver::resolve(&fixture_path("dockerfile-mode"))
        .await
        .unwrap();

    // Should read the actual Dockerfile content
    assert!(config.dockerfile.contains("FROM node:20"));
    assert!(config.dockerfile.contains("apt-get update"));
    assert_eq!(config.remote_user.as_deref(), Some("developer"));
    assert_eq!(config.forwarded_ports, vec![4000]);

    assert_eq!(config.post_create_commands.len(), 1);
    assert!(matches!(&config.post_create_commands[0], Command::Shell(s) if s == "npm install"));

    // build.args
    assert_eq!(
        config.build_args.get("NODE_VERSION").map(String::as_str),
        Some("20")
    );

    // build.target
    assert_eq!(config.build_target.as_deref(), Some("dev"));
}

#[tokio::test]
async fn resolve_compose_mode() {
    let config = DevcontainerResolver::resolve(&fixture_path("compose-mode"))
        .await
        .unwrap();

    // In compose mode, the dockerfile is derived from the compose service's image
    assert!(config.dockerfile.contains("FROM node:20"));
    assert_eq!(config.workspace_folder, "/workspace");
    assert_eq!(config.remote_user.as_deref(), Some("node"));
    assert_eq!(config.compose_files.len(), 1);
    assert_eq!(config.compose_service.as_deref(), Some("app"));

    // Ports come from compose + forwardPorts merged
    assert_eq!(config.forwarded_ports, vec![3000, 9229, 5173]);

    // Environment merged from compose + remoteEnv
    assert_eq!(
        config.environment.get("NODE_ENV").map(String::as_str),
        Some("development")
    );
    assert_eq!(
        config.environment.get("DEBUG").map(String::as_str),
        Some("true")
    );
}

#[tokio::test]
async fn resolve_variables() {
    let config = DevcontainerResolver::resolve(&fixture_path("variables"))
        .await
        .unwrap();

    assert_eq!(config.workspace_folder, "/workspaces/variables");
    assert_eq!(
        config.environment.get("PROJECT_ROOT").map(String::as_str),
        Some("/workspaces/variables")
    );
    assert_eq!(
        config.environment.get("PROJECT_NAME").map(String::as_str),
        Some("variables")
    );
}

#[tokio::test]
async fn resolve_compose_multi() {
    let config = DevcontainerResolver::resolve(&fixture_path("compose-multi"))
        .await
        .unwrap();

    // Override file wins for image
    assert!(config.dockerfile.contains("FROM node:22"));
    assert_eq!(config.workspace_folder, "/workspace");
    assert_eq!(config.compose_files.len(), 2);
    assert_eq!(config.compose_service.as_deref(), Some("app"));

    // Port from base.yml
    assert_eq!(config.forwarded_ports, vec![3000]);

    // Environment from override.yml
    assert_eq!(
        config.environment.get("OVERRIDE_VAR").map(String::as_str),
        Some("true")
    );
}

#[tokio::test]
async fn resolve_not_found() {
    let result = DevcontainerResolver::resolve(&fixture_path("nonexistent")).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("no devcontainer.json found"));
}

#[tokio::test]
async fn resolve_subdirectory_mode() {
    let config = DevcontainerResolver::resolve(&fixture_path("subdirectory-mode"))
        .await
        .unwrap();

    assert!(
        config
            .dockerfile
            .contains("FROM mcr.microsoft.com/devcontainers/python:3.12")
    );
    assert_eq!(config.remote_user.as_deref(), Some("vscode"));
    assert_eq!(config.workspace_folder, "/workspaces/subdirectory-mode");
}

#[tokio::test]
async fn resolve_subdirectory_multiple_picks_alphabetical_first() {
    let config = DevcontainerResolver::resolve(&fixture_path("subdirectory-multiple"))
        .await
        .unwrap();

    // "alpha" sorts before "beta", so alpha's config is used
    assert!(
        config
            .dockerfile
            .contains("FROM mcr.microsoft.com/devcontainers/base:ubuntu")
    );
    assert_eq!(config.remote_user.as_deref(), Some("alpha-user"));
}

#[tokio::test]
async fn resolve_subdirectory_standard_wins_over_subdirs() {
    let config = DevcontainerResolver::resolve(&fixture_path("subdirectory-with-standard"))
        .await
        .unwrap();

    // Standard .devcontainer/devcontainer.json takes priority over subdirectory format
    assert!(
        config
            .dockerfile
            .contains("FROM mcr.microsoft.com/devcontainers/base:ubuntu")
    );
    assert_eq!(config.remote_user.as_deref(), Some("standard-user"));
}

#[tokio::test]
async fn generated_dockerfile_is_well_formed() {
    let config = DevcontainerResolver::resolve(&fixture_path("image-only"))
        .await
        .unwrap();

    // Should start with the generated header
    assert!(
        config
            .dockerfile
            .contains("# Generated by fabro-devcontainer")
    );
    // Should have the base image
    assert!(config.dockerfile.contains("FROM"));
    // Should end with a newline
    assert!(config.dockerfile.ends_with('\n'));
}
