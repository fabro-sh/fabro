//! End-to-end tests exercising full resolver pipeline with realistic
//! devcontainer configs. These tests verify the 4 critical gaps are wired
//! correctly through the entire stack:
//!   1. onCreateCommand
//!   2. build.args
//!   3. containerEnv
//!   4. dockerComposeFile array

use fabro_devcontainer::{Command, DevcontainerResolver};

use super::helpers::fixture_path;

/// Realistic Python project: Dockerfile + build.args + containerEnv +
/// onCreateCommand + remoteEnv Verifies all 4 gaps work together in a single
/// config.
#[tokio::test]
async fn realistic_python_project() {
    let config = DevcontainerResolver::resolve(&fixture_path("realistic-python"))
        .await
        .unwrap();

    // Gap 2: build.args exposed for docker build --build-arg
    assert_eq!(
        config.build_args.get("PYTHON_VERSION").map(String::as_str),
        Some("3.12")
    );

    // Gap 3: containerEnv baked into Dockerfile as ENV directives
    assert!(config.dockerfile.contains("ENV PIP_NO_CACHE_DIR=1"));
    assert!(config.dockerfile.contains("ENV PYTHONDONTWRITEBYTECODE=1"));
    assert_eq!(
        config
            .container_env
            .get("PIP_NO_CACHE_DIR")
            .map(String::as_str),
        Some("1")
    );

    // After fix: only containerEnv is baked into Dockerfile (remoteEnv is
    // runtime-only)
    assert!(config.dockerfile.contains("ENV PYTHONUNBUFFERED=1"));
    // environment HashMap gets the remoteEnv value
    assert_eq!(
        config
            .environment
            .get("PYTHONUNBUFFERED")
            .map(String::as_str),
        Some("yes")
    );

    // Gap 3: remoteEnv with variable substitution
    assert_eq!(
        config.environment.get("PYTHONPATH").map(String::as_str),
        Some("/workspaces/realistic-python/src")
    );

    // Gap 1: onCreateCommand parsed and exposed
    assert_eq!(config.on_create_commands.len(), 1);
    assert!(
        matches!(&config.on_create_commands[0], Command::Shell(s) if s == "pip install -r requirements.txt")
    );

    // Other lifecycle commands still work
    assert_eq!(config.post_create_commands.len(), 1);
    assert!(
        matches!(&config.post_create_commands[0], Command::Shell(s) if s == "python manage.py migrate")
    );
    assert_eq!(config.post_start_commands.len(), 1);
    assert!(
        matches!(&config.post_start_commands[0], Command::Shell(s) if s == "python manage.py runserver 0.0.0.0:8000")
    );

    // Dockerfile content is the actual file (not generated FROM line)
    assert!(config.dockerfile.contains("ARG PYTHON_VERSION=3.11"));
    assert!(config.dockerfile.contains("apt-get update"));

    // Standard fields
    assert_eq!(config.remote_user.as_deref(), Some("developer"));
    assert_eq!(config.forwarded_ports, vec![8000, 5432]);
    assert!(config.compose_files.is_empty());
}

/// Realistic compose project: multi-file compose + containerEnv +
/// onCreateCommand + remoteEnv Verifies gaps 1, 3, 4 work together in compose
/// mode.
#[tokio::test]
async fn realistic_compose_project() {
    let config = DevcontainerResolver::resolve(&fixture_path("realistic-compose"))
        .await
        .unwrap();

    // Gap 4: multiple compose files resolved
    assert_eq!(config.compose_files.len(), 2);
    assert_eq!(config.compose_service.as_deref(), Some("app"));

    // Gap 4: image from base compose file (override doesn't change image)
    assert!(config.dockerfile.contains("FROM node:20-bookworm"));

    // Ports merged from both compose files (base: 3000, 9229; override: 4000) +
    // forwardPorts (8080)
    assert!(config.forwarded_ports.contains(&3000));
    assert!(config.forwarded_ports.contains(&9229));
    assert!(config.forwarded_ports.contains(&4000));
    assert!(config.forwarded_ports.contains(&8080));
    assert_eq!(config.forwarded_ports.len(), 4);

    // Gap 4: environment merged from both compose files + remoteEnv
    assert_eq!(
        config.environment.get("NODE_ENV").map(String::as_str),
        Some("development")
    );
    assert_eq!(
        config.environment.get("DEBUG").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        config.environment.get("LOG_LEVEL").map(String::as_str),
        Some("verbose")
    );
    // remoteEnv values
    assert_eq!(
        config.environment.get("DATABASE_URL").map(String::as_str),
        Some("postgres://postgres:devpass@db:5432/myapp_dev")
    );
    assert_eq!(
        config.environment.get("REDIS_URL").map(String::as_str),
        Some("redis://redis:6379")
    );

    // Gap 3: containerEnv exposed on config
    assert_eq!(
        config.container_env.get("TERM").map(String::as_str),
        Some("xterm-256color")
    );
    assert_eq!(
        config.container_env.get("EDITOR").map(String::as_str),
        Some("vim")
    );

    // Gap 1: onCreateCommand in compose mode
    assert_eq!(config.on_create_commands.len(), 1);
    assert!(matches!(&config.on_create_commands[0], Command::Shell(s) if s == "npm ci"));

    // Other lifecycle commands
    assert_eq!(config.post_create_commands.len(), 1);
    assert!(
        matches!(&config.post_create_commands[0], Command::Shell(s) if s == "npm run db:migrate")
    );
    assert_eq!(config.post_start_commands.len(), 1);
    assert!(matches!(&config.post_start_commands[0], Command::Shell(s) if s == "npm run dev"));

    // User comes from compose (node) but remoteUser also set to node
    assert_eq!(config.remote_user.as_deref(), Some("node"));
    assert_eq!(config.workspace_folder, "/workspace");
}

/// All lifecycle commands in different forms: string, array, object, and the
/// new onCreateCommand.
#[tokio::test]
async fn all_lifecycle_command_forms() {
    let config = DevcontainerResolver::resolve(&fixture_path("all-lifecycle"))
        .await
        .unwrap();

    // initializeCommand as string
    assert_eq!(config.initialize_commands.len(), 1);
    assert!(matches!(&config.initialize_commands[0], Command::Shell(s) if s == "echo pre-build"));

    // Gap 1: onCreateCommand as array
    assert_eq!(config.on_create_commands.len(), 1);
    assert!(
        matches!(&config.on_create_commands[0], Command::Args(args) if args == &["make", "setup"])
    );

    // postCreateCommand as object (parallel)
    assert_eq!(config.post_create_commands.len(), 1);
    assert!(matches!(&config.post_create_commands[0], Command::Parallel(map) if map.len() == 2));

    // postStartCommand as string
    assert_eq!(config.post_start_commands.len(), 1);
    assert!(matches!(&config.post_start_commands[0], Command::Shell(s) if s == "echo started"));
}

/// Verify containerEnv doesn't pollute the environment HashMap (which is
/// remoteEnv only).
#[tokio::test]
async fn container_env_separate_from_environment() {
    let config = DevcontainerResolver::resolve(&fixture_path("realistic-python"))
        .await
        .unwrap();

    // container_env has containerEnv values
    assert!(config.container_env.contains_key("PYTHONDONTWRITEBYTECODE"));
    assert!(config.container_env.contains_key("PIP_NO_CACHE_DIR"));

    // environment only has remoteEnv values (not containerEnv-only keys)
    assert!(!config.environment.contains_key("PYTHONDONTWRITEBYTECODE"));
    assert!(!config.environment.contains_key("PIP_NO_CACHE_DIR"));
    // PYTHONUNBUFFERED is in both - environment gets remoteEnv value
    assert_eq!(
        config
            .environment
            .get("PYTHONUNBUFFERED")
            .map(String::as_str),
        Some("yes")
    );
}

/// Verify build_args default to empty in non-dockerfile modes.
#[tokio::test]
async fn build_args_empty_in_image_and_compose_modes() {
    let image_config = DevcontainerResolver::resolve(&fixture_path("image-only"))
        .await
        .unwrap();
    assert!(image_config.build_args.is_empty());

    let compose_config = DevcontainerResolver::resolve(&fixture_path("compose-mode"))
        .await
        .unwrap();
    assert!(compose_config.build_args.is_empty());
}

/// Verify build_target is None for image-only and compose modes.
#[tokio::test]
async fn build_target_none_in_image_and_compose_modes() {
    let image_config = DevcontainerResolver::resolve(&fixture_path("image-only"))
        .await
        .unwrap();
    assert!(image_config.build_target.is_none());

    let compose_config = DevcontainerResolver::resolve(&fixture_path("compose-mode"))
        .await
        .unwrap();
    assert!(compose_config.build_target.is_none());
}

/// Gap 1: remoteEnv values must NOT appear as ENV directives in the generated
/// Dockerfile. Only containerEnv should be baked in.
#[tokio::test]
async fn remote_env_excluded_from_dockerfile() {
    // image-only fixture has remoteEnv: {"EDITOR": "code"} and containerEnv:
    // {"DEBIAN_FRONTEND": "noninteractive"}
    let config = DevcontainerResolver::resolve(&fixture_path("image-only"))
        .await
        .unwrap();

    // containerEnv IS in the Dockerfile
    assert!(
        config
            .dockerfile
            .contains("ENV DEBIAN_FRONTEND=noninteractive")
    );

    // remoteEnv is NOT in the Dockerfile
    assert!(!config.dockerfile.contains("EDITOR=code"));

    // remoteEnv IS in the environment HashMap (runtime-only)
    assert_eq!(
        config.environment.get("EDITOR").map(String::as_str),
        Some("code")
    );
}

/// Gap 2: forwardPorts in compose mode are merged with compose service ports,
/// with deduplication.
#[tokio::test]
async fn forward_ports_merged_and_deduped_in_compose() {
    // compose-mode fixture has compose ports [3000, 9229] and forwardPorts [3000,
    // 5173]
    let config = DevcontainerResolver::resolve(&fixture_path("compose-mode"))
        .await
        .unwrap();

    // 3000 appears in both compose ports and forwardPorts — should NOT be
    // duplicated
    assert_eq!(config.forwarded_ports, vec![3000, 9229, 5173]);
}

/// Gap 3: build.target is parsed and exposed in dockerfile mode.
#[tokio::test]
async fn build_target_in_dockerfile_mode() {
    let config = DevcontainerResolver::resolve(&fixture_path("dockerfile-mode"))
        .await
        .unwrap();

    assert_eq!(config.build_target.as_deref(), Some("dev"));
}

/// Gap 4: forwardPorts string formats ("host:container", "port") are parsed
/// correctly.
#[tokio::test]
async fn forward_ports_string_formats() {
    // image-only fixture has forwardPorts: [3000, "8080:80", "9090"]
    let config = DevcontainerResolver::resolve(&fixture_path("image-only"))
        .await
        .unwrap();

    // 3000 is a plain number
    assert!(config.forwarded_ports.contains(&3000));
    // "8080:80" extracts container port 80
    assert!(config.forwarded_ports.contains(&80));
    // "9090" is parsed as a plain port number
    assert!(config.forwarded_ports.contains(&9090));
    // host port 8080 should NOT appear (only container port matters)
    assert!(!config.forwarded_ports.contains(&8080));

    assert_eq!(config.forwarded_ports, vec![3000, 80, 9090]);
}

/// Verify compose_files is empty for non-compose modes.
#[tokio::test]
async fn compose_files_empty_in_non_compose_modes() {
    let image_config = DevcontainerResolver::resolve(&fixture_path("image-only"))
        .await
        .unwrap();
    assert!(image_config.compose_files.is_empty());

    let df_config = DevcontainerResolver::resolve(&fixture_path("dockerfile-mode"))
        .await
        .unwrap();
    assert!(df_config.compose_files.is_empty());
}

/// Verify on_create_commands defaults to empty when not specified.
#[tokio::test]
async fn on_create_commands_empty_when_not_specified() {
    let config = DevcontainerResolver::resolve(&fixture_path("variables"))
        .await
        .unwrap();
    assert!(config.on_create_commands.is_empty());
}

/// Verify container_env defaults to empty when not specified.
#[tokio::test]
async fn container_env_empty_when_not_specified() {
    let config = DevcontainerResolver::resolve(&fixture_path("variables"))
        .await
        .unwrap();
    assert!(config.container_env.is_empty());
}

// === Gap e2e tests: local features exercising dependsOn, containerEnv,
// lifecycle hooks ===

/// Gap 5: Local path feature references are resolved through the full pipeline.
#[tokio::test]
async fn local_feature_refs_resolved() {
    let config = DevcontainerResolver::resolve(&fixture_path("local-features"))
        .await
        .unwrap();

    // Base image preserved
    assert!(
        config
            .dockerfile
            .contains("FROM mcr.microsoft.com/devcontainers/base:ubuntu")
    );

    // Feature install.sh snippets are in the Dockerfile
    assert!(config.dockerfile.contains("node-feature"));
    assert!(config.dockerfile.contains("python-feature"));

    // Node feature option "version=20" passed as env var
    assert!(config.dockerfile.contains("export VERSION=\"20\""));
}

/// Gap 1: dependsOn auto-injects missing features through the full pipeline.
/// node-feature dependsOn ./base-utils which is NOT listed in devcontainer.json
/// features.
#[tokio::test]
async fn depends_on_auto_injects_missing_feature() {
    let config = DevcontainerResolver::resolve(&fixture_path("local-features"))
        .await
        .unwrap();

    // base-utils was auto-injected and its install.sh snippet is in the Dockerfile
    assert!(config.dockerfile.contains("base-utils"));

    // base-utils must appear before node-feature (dependency ordering)
    let base_pos = config.dockerfile.find("base-utils").unwrap();
    let node_pos = config.dockerfile.find("node-feature").unwrap();
    assert!(
        base_pos < node_pos,
        "base-utils (pos {base_pos}) should appear before node-feature (pos {node_pos})"
    );
}

/// Gap 2: Feature containerEnv is merged into the Dockerfile and config.
#[tokio::test]
async fn feature_container_env_merged() {
    let config = DevcontainerResolver::resolve(&fixture_path("local-features"))
        .await
        .unwrap();

    // Feature containerEnv values baked into Dockerfile
    assert!(config.dockerfile.contains("ENV NODE_INSTALLED=true"));
    assert!(
        config
            .dockerfile
            .contains("ENV NODE_PATH=/usr/local/lib/node_modules")
    );
    assert!(config.dockerfile.contains("ENV PYTHON_INSTALLED=true"));
    assert!(config.dockerfile.contains("ENV BASE_UTILS_INSTALLED=true"));

    // Devcontainer.json containerEnv also present
    assert!(config.dockerfile.contains("ENV DEVCONTAINER=true"));

    // All values in config.container_env
    assert_eq!(
        config
            .container_env
            .get("NODE_INSTALLED")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        config
            .container_env
            .get("PYTHON_INSTALLED")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        config
            .container_env
            .get("BASE_UTILS_INSTALLED")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        config.container_env.get("DEVCONTAINER").map(String::as_str),
        Some("true")
    );
}

/// Gap 3: Feature lifecycle hooks are appended after devcontainer.json
/// lifecycle commands.
#[tokio::test]
async fn feature_lifecycle_hooks_appended() {
    let config = DevcontainerResolver::resolve(&fixture_path("local-features"))
        .await
        .unwrap();

    // onCreateCommand: devcontainer.json first, then features
    // devcontainer.json: "echo devcontainer-setup"
    // base-utils: "echo base-utils-setup"
    // node-feature: "echo node-setup"
    assert!(config.on_create_commands.len() >= 2);
    assert!(
        matches!(&config.on_create_commands[0], Command::Shell(s) if s == "echo devcontainer-setup")
    );

    // Feature on_create_commands appear after devcontainer.json's
    let feature_on_create: Vec<&str> = config.on_create_commands[1..]
        .iter()
        .filter_map(|cmd| match cmd {
            Command::Shell(s) => Some(s.as_str()),
            _ => None,
        })
        .collect();
    assert!(feature_on_create.contains(&"echo base-utils-setup"));
    assert!(feature_on_create.contains(&"echo node-setup"));

    // postCreateCommand: devcontainer.json first, then python-feature
    assert!(config.post_create_commands.len() >= 2);
    assert!(
        matches!(&config.post_create_commands[0], Command::Shell(s) if s == "echo devcontainer-post-create")
    );
    let feature_post_create: Vec<&str> = config.post_create_commands[1..]
        .iter()
        .filter_map(|cmd| match cmd {
            Command::Shell(s) => Some(s.as_str()),
            _ => None,
        })
        .collect();
    assert!(feature_post_create.contains(&"echo python-post-create"));

    // postStartCommand: only node-feature contributes (no devcontainer.json
    // postStartCommand)
    assert!(!config.post_start_commands.is_empty());
    let post_start: Vec<&str> = config
        .post_start_commands
        .iter()
        .filter_map(|cmd| match cmd {
            Command::Shell(s) => Some(s.as_str()),
            _ => None,
        })
        .collect();
    assert!(post_start.contains(&"echo node-started"));
}

/// Fix 1: Shorthand version syntax "1.21" is normalized to {"version": "1.21"}.
#[tokio::test]
async fn feature_shorthand_version_syntax() {
    let config = DevcontainerResolver::resolve(&fixture_path("feature-options"))
        .await
        .unwrap();

    // "1.21" string should become version=1.21 env var
    assert!(
        config.dockerfile.contains("export VERSION=\"1.21\""),
        "shorthand string \"1.21\" should set VERSION env var, got:\n{}",
        config.dockerfile,
    );
}

/// Fix 2: Hyphenated option IDs are converted to valid env var names
/// (node-version → NODE_VERSION).
#[tokio::test]
async fn feature_option_id_hyphen_to_underscore() {
    let config = DevcontainerResolver::resolve(&fixture_path("feature-options"))
        .await
        .unwrap();

    // node-version default "none" should export as NODE_VERSION (not NODE-VERSION)
    assert!(
        config.dockerfile.contains("export NODE_VERSION=\"none\""),
        "hyphenated option 'node-version' should become NODE_VERSION env var, got:\n{}",
        config.dockerfile,
    );
    assert!(
        !config.dockerfile.contains("NODE-VERSION"),
        "NODE-VERSION (with hyphen) should not appear in Dockerfile",
    );
}

/// Fix 3: _REMOTE_USER and related env vars are emitted in feature install
/// snippets.
#[tokio::test]
async fn feature_install_user_env_vars() {
    let config = DevcontainerResolver::resolve(&fixture_path("feature-options"))
        .await
        .unwrap();

    // remoteUser is "developer", so _REMOTE_USER should be "developer"
    assert!(
        config.dockerfile.contains("_REMOTE_USER=\"developer\""),
        "_REMOTE_USER should be set to remoteUser value, got:\n{}",
        config.dockerfile,
    );
    assert!(
        config.dockerfile.contains("_CONTAINER_USER=\"root\""),
        "_CONTAINER_USER should always be root",
    );
    assert!(
        config
            .dockerfile
            .contains("_REMOTE_USER_HOME=\"/home/developer\""),
        "_REMOTE_USER_HOME should be /home/developer",
    );
    assert!(
        config.dockerfile.contains("_CONTAINER_USER_HOME=\"/root\""),
        "_CONTAINER_USER_HOME should always be /root",
    );
}

/// Fix 3: _REMOTE_USER defaults to root when remoteUser is not set.
#[tokio::test]
async fn feature_install_user_env_vars_default_root() {
    let config = DevcontainerResolver::resolve(&fixture_path("local-features"))
        .await
        .unwrap();

    // local-features has remoteUser: "vscode"
    assert!(
        config.dockerfile.contains("_REMOTE_USER=\"vscode\""),
        "_REMOTE_USER should be set to vscode, got:\n{}",
        config.dockerfile,
    );
    assert!(
        config
            .dockerfile
            .contains("_REMOTE_USER_HOME=\"/home/vscode\""),
        "_REMOTE_USER_HOME should be /home/vscode",
    );
}

/// Gap 2+3: Feature ordering affects both containerEnv and lifecycle hook
/// collection. python-feature installsAfter node-feature, so node's env/hooks
/// come first.
#[tokio::test]
async fn feature_ordering_preserved_in_env_and_hooks() {
    let config = DevcontainerResolver::resolve(&fixture_path("local-features"))
        .await
        .unwrap();

    // In the Dockerfile, node-feature layers come before python-feature layers
    let node_layer_pos = config.dockerfile.find("node-feature").unwrap();
    let python_layer_pos = config.dockerfile.find("python-feature").unwrap();
    assert!(
        node_layer_pos < python_layer_pos,
        "node-feature (pos {node_layer_pos}) should be installed before python-feature (pos {python_layer_pos})"
    );
}
