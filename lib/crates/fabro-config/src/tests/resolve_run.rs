use fabro_types::settings::InterpString;
use fabro_types::settings::run::{
    ApprovalMode, EnvironmentNetworkMode, EnvironmentProvider, RunGoal, RunMode,
};

use crate::{MergeMap, SettingsLayer};

fn catalog(source: &str) -> MergeMap<crate::EnvironmentLayer> {
    source
        .parse::<SettingsLayer>()
        .expect("environment catalog should parse")
        .environments
}

fn workflow_settings_from_toml_with_catalog(
    source: &str,
    catalog: &str,
) -> crate::Result<fabro_types::WorkflowSettings> {
    super::workflow_settings_from_toml_with_catalog(source, self::catalog(catalog))
}

fn workflow_settings_from_toml(source: &str) -> crate::Result<fabro_types::WorkflowSettings> {
    super::workflow_settings_from_toml(source)
}

fn workflow_settings_from_layer(
    layer: SettingsLayer,
) -> std::result::Result<fabro_types::WorkflowSettings, crate::ResolveErrors> {
    super::workflow_settings_from_layer(layer)
}

#[test]
fn run_model_controls_round_trip_through_resolve() {
    let settings = super::workflow_settings_from_toml(
        r#"
_version = 1

[run.model.controls]
reasoning_effort = "high"
speed = "fast"
"#,
    )
    .expect("[run.model.controls] should resolve")
    .run;

    assert_eq!(
        settings.model.controls.reasoning_effort.as_deref(),
        Some("high")
    );
    assert_eq!(settings.model.controls.speed.as_deref(), Some("fast"));
}

#[test]
fn run_model_controls_default_to_none() {
    let settings = super::workflow_settings_from_layer(SettingsLayer::default())
        .expect("empty settings should resolve")
        .run;

    assert!(settings.model.controls.reasoning_effort.is_none());
    assert!(settings.model.controls.speed.is_none());
}

#[test]
fn resolves_run_defaults_from_empty_settings() {
    let settings = super::workflow_settings_from_layer(SettingsLayer::default())
        .expect("empty settings should resolve")
        .run;

    assert_eq!(settings.execution.mode, RunMode::Normal);
    assert_eq!(settings.execution.approval, ApprovalMode::Prompt);
    assert_eq!(settings.prepare.timeout_ms, 300_000);
    assert_eq!(settings.environment.id, "default");
    assert_eq!(settings.environment.provider, EnvironmentProvider::Docker);
    assert_eq!(
        settings.environment.image.docker.as_deref(),
        Some("buildpack-deps:noble")
    );
    assert_eq!(settings.environment.resources.cpu, Some(2));
    assert_eq!(
        settings
            .environment
            .resources
            .memory
            .map(|size| size.as_bytes()),
        Some(4_000_000_000)
    );
    assert!(!settings.environment.lifecycle.preserve);
    assert!(settings.environment.lifecycle.stop_on_terminal);
    assert!(settings.clone.enabled);
    assert!(settings.run_branch.enabled);
    assert!(settings.run_branch.push);
    assert!(settings.meta_branch.enabled);
    assert!(settings.meta_branch.push);
    assert!(settings.pull_request.is_none());
}

#[expect(
    clippy::disallowed_methods,
    reason = "test asserts the raw template source"
)]
#[test]
fn resolves_named_daytona_environment_from_injected_catalog() {
    let settings = workflow_settings_from_toml_with_catalog(
        r#"
_version = 1

[run.environment]
id = "fabro-dev"
"#,
        r#"
[environments.fabro-dev]
provider = "daytona"

[environments.fabro-dev.image]
dockerfile = "FROM ubuntu:24.04"

[environments.fabro-dev.resources]
cpu = 8
memory = "16GB"
disk = "20GB"

[environments.fabro-dev.network]
mode = "cidr_allow_list"
allow = ["10.0.0.0/8"]

[environments.fabro-dev.lifecycle]
preserve = false
stop_on_terminal = true
auto_stop = "30m"

[environments.fabro-dev.labels]
repo = "fabro-sh/fabro"

[[environments.fabro-dev.volumes]]
id = "vol_auth"
mount_path = "/home/daytona/.config"
subpath = "agents"

[environments.fabro-dev.env]
NODE_ENV = "development"
"#,
    )
    .expect("daytona environment should resolve");

    let environment = settings.run.environment;

    assert_eq!(environment.id, "fabro-dev");
    assert_eq!(environment.provider, EnvironmentProvider::Daytona);
    assert_eq!(environment.image.docker.as_deref(), None);
    assert!(environment.image.dockerfile.is_some());
    assert_eq!(environment.resources.cpu, Some(8));
    assert_eq!(
        environment.resources.memory.map(|size| size.as_bytes()),
        Some(16_000_000_000)
    );
    assert_eq!(
        environment.resources.disk.map(|size| size.as_bytes()),
        Some(20_000_000_000)
    );
    assert_eq!(
        environment.network.mode,
        EnvironmentNetworkMode::CidrAllowList
    );
    assert_eq!(environment.network.allow, vec!["10.0.0.0/8"]);
    assert!(!environment.lifecycle.preserve);
    assert_eq!(
        environment
            .lifecycle
            .auto_stop
            .map(|duration| duration.as_std().as_secs()),
        Some(1800)
    );
    assert_eq!(
        environment.labels.get("repo").map(String::as_str),
        Some("fabro-sh/fabro")
    );
    assert_eq!(environment.volumes.len(), 1);
    assert_eq!(environment.volumes[0].id, "vol_auth");
    assert_eq!(environment.volumes[0].mount_path, "/home/daytona/.config");
    assert_eq!(environment.volumes[0].subpath.as_deref(), Some("agents"));
    assert_eq!(
        environment
            .env
            .get("NODE_ENV")
            .map(InterpString::as_source)
            .as_deref(),
        Some("development")
    );
}

#[test]
fn resolves_run_level_clone_branch_controls() {
    let settings = super::workflow_settings_from_toml(
        r"
_version = 1

[run.clone]
enabled = false

[run.run_branch]
enabled = true
push = false

[run.meta_branch]
enabled = true
push = false
",
    )
    .expect("run branch controls should resolve")
    .run;

    assert!(!settings.clone.enabled);
    assert!(settings.run_branch.enabled);
    assert!(!settings.run_branch.push);
    assert!(settings.meta_branch.enabled);
    assert!(!settings.meta_branch.push);
}

#[test]
fn disabling_run_branch_forces_meta_branch_off() {
    let settings = super::workflow_settings_from_toml(
        r"
_version = 1

[run.run_branch]
enabled = false

[run.meta_branch]
enabled = true
push = true
",
    )
    .expect("run branch disabled should resolve")
    .run;

    assert!(!settings.run_branch.enabled);
    assert!(!settings.meta_branch.enabled);
    assert!(!settings.meta_branch.push);
}

#[test]
fn pull_request_requires_pushed_run_branch() {
    let disabled_branch = super::workflow_settings_from_toml(
        r"
_version = 1

[run.run_branch]
enabled = false

[run.pull_request]
enabled = true
",
    )
    .expect_err("pull requests require an enabled pushed run branch");
    let message = disabled_branch.to_string();
    assert!(
        message.contains("run.pull_request.enabled requires run.run_branch.enabled"),
        "expected run branch validation error, got: {message}"
    );

    let disabled_push = super::workflow_settings_from_toml(
        r"
_version = 1

[run.run_branch]
push = false

[run.pull_request]
enabled = true
",
    )
    .expect_err("pull requests require run branch push");
    let message = disabled_push.to_string();
    assert!(
        message.contains("run.pull_request.enabled requires run.run_branch.enabled"),
        "expected run branch push validation error, got: {message}"
    );
}

#[test]
fn legacy_run_sandbox_is_rejected() {
    let err = r#"
_version = 1

[run.sandbox]
provider = "local"
"#
    .parse::<SettingsLayer>()
    .expect_err("legacy run.sandbox should be unknown");
    let message = err.to_string();
    assert!(
        message.contains("sandbox") || message.contains("unknown field"),
        "expected unknown-field error mentioning sandbox, got: {message}"
    );
}

#[test]
fn resolved_run_chat_surfaces_are_slack_only() {
    let settings = super::workflow_settings_from_toml(
        r##"
_version = 1

[run.notifications.ops]
enabled = true
provider = "slack"
events = ["run.completed"]

[run.notifications.ops.slack]
channel = "#ops"

[run.interviews]
provider = "slack"

[run.interviews.slack]
channel = "#ops"
"##,
    )
    .expect("slack-only chat settings should resolve")
    .run;

    let route = settings
        .notifications
        .get("ops")
        .expect("notification route should resolve");

    assert_eq!(
        serde_json::to_value(route).expect("route should serialize"),
        serde_json::json!({
            "enabled": true,
            "provider": "slack",
            "events": ["run.completed"],
            "slack": {
                "channel": "#ops",
            },
        })
    );
    assert_eq!(
        serde_json::to_value(&settings.interviews).expect("interviews should serialize"),
        serde_json::json!({
            "provider": "slack",
            "slack": {
                "channel": "#ops",
            },
        })
    );
}

#[test]
fn parsing_rejects_unknown_run_chat_destinations() {
    let notifications = r##"
_version = 1

[run.notifications.ops.chatapp]
channel = "#ops"
"##;

    let err = notifications
        .parse::<SettingsLayer>()
        .expect_err("unknown notification destination should be rejected");
    let message = err.to_string();
    assert!(
        message.contains("chatapp") || message.contains("unknown field"),
        "expected notification parse error for unknown chat provider, got: {message}"
    );

    let interviews = r##"
_version = 1

[run.interviews.chatapp]
channel = "#ops"
"##;

    let err = interviews
        .parse::<SettingsLayer>()
        .expect_err("unknown interview destination should be rejected");
    let message = err.to_string();
    assert!(
        message.contains("chatapp") || message.contains("unknown field"),
        "expected interview parse error for unknown chat provider, got: {message}"
    );
}

#[test]
fn toml_run_environment_lifecycle_override_is_applied() {
    let settings = super::workflow_settings_from_toml(
        r"
_version = 1

[run.environment.lifecycle]
stop_on_terminal = false
",
    )
    .expect("TOML environment lifecycle overrides should resolve");

    assert!(!settings.run.environment.lifecycle.stop_on_terminal);
}

#[test]
fn resolves_minimal_local_environment() {
    let settings = workflow_settings_from_toml_with_catalog(
        r#"
_version = 1

[run.environment]
id = "host"
"#,
        r#"
[environments.host]
provider = "local"
"#,
    )
    .expect("minimal local environment settings should resolve")
    .run;

    assert_eq!(settings.environment.id, "host");
    assert_eq!(settings.environment.provider, EnvironmentProvider::Local);
    assert!(settings.environment.image.docker.is_none());
}

#[test]
fn missing_environment_slug_errors() {
    let err = super::workflow_settings_from_toml(
        r#"
_version = 1

[run.environment]
id = "missing"
"#,
    )
    .expect_err("missing selected environment should error");

    let message = err.to_string();
    assert!(
        message.contains("run.environment.id") && message.contains("missing"),
        "expected missing environment diagnostic, got: {message}"
    );
}

#[test]
fn docker_cidr_allow_list_errors() {
    let err = workflow_settings_from_toml_with_catalog(
        r#"
_version = 1

[run.environment]
id = "locked"
"#,
        r#"
[environments.locked]
provider = "docker"

[environments.locked.network]
mode = "cidr_allow_list"
allow = ["10.0.0.0/8"]
"#,
    )
    .expect_err("docker cannot enforce cidr allow list");

    let message = err.to_string();
    assert!(
        message.contains("run.environment.network.mode") && message.contains("CIDR allow-list"),
        "expected docker CIDR capability diagnostic, got: {message}"
    );
}

#[test]
fn local_blocked_network_errors() {
    let err = workflow_settings_from_toml_with_catalog(
        r#"
_version = 1

[run.environment]
id = "host"
"#,
        r#"
[environments.host]
provider = "local"

[environments.host.network]
mode = "block"
"#,
    )
    .expect_err("local cannot enforce blocked networking");

    let message = err.to_string();
    assert!(
        message.contains("run.environment.network.mode")
            && message.contains("local environments cannot enforce"),
        "expected local blocked-network diagnostic, got: {message}"
    );
}

#[test]
fn daytona_dockerfile_without_image_ref_resolves() {
    let settings = workflow_settings_from_toml_with_catalog(
        r#"
_version = 1

[run.environment]
id = "cloud"
"#,
        r#"
[environments.cloud]
provider = "daytona"

[environments.cloud.image]
dockerfile = { path = "Dockerfile" }
"#,
    )
    .expect("daytona dockerfile should not need a user-supplied snapshot name")
    .run;

    assert_eq!(settings.environment.provider, EnvironmentProvider::Daytona);
    assert!(settings.environment.image.docker.is_none());
    assert!(settings.environment.image.dockerfile.is_some());
}

#[test]
fn daytona_image_docker_errors() {
    let err = workflow_settings_from_toml_with_catalog(
        r#"
_version = 1

[run.environment]
id = "cloud"
"#,
        r#"
[environments.cloud]
provider = "daytona"

[environments.cloud.image]
docker = "ubuntu:24.04"
"#,
    )
    .expect_err("daytona should reject docker image selection");

    let message = err.to_string();
    assert!(
        message.contains("image.docker") && message.contains("daytona"),
        "expected daytona image.docker diagnostic, got: {message}"
    );
}

#[test]
fn image_ref_is_rejected_as_unknown_field() {
    let err = super::workflow_settings_from_toml(
        r#"
_version = 1

[run.environment.image]
ref = "ubuntu:24.04"
"#,
    )
    .expect_err("image.ref should not be accepted");

    let message = err.to_string();
    assert!(
        message.contains("unknown field") && message.contains("ref"),
        "expected unknown field diagnostic for image.ref, got: {message}"
    );
}

#[test]
fn preserves_goal_variants_and_model_sources() {
    let settings = super::workflow_settings_from_toml(
        r#"
_version = 1

[run]
working_dir = "{{ env.FABRO_WORKDIR }}"

[run.goal]
file = "{{ env.GOAL_FILE }}"

[run.model]
provider = "anthropic"
name = "sonnet"
"#,
    )
    .expect("run settings should resolve")
    .run;

    match settings.goal {
        Some(RunGoal::File(path)) => {
            assert_eq!(path, InterpString::parse("{{ env.GOAL_FILE }}"));
        }
        other => panic!("expected file goal, got {other:?}"),
    }
    assert_eq!(
        settings.working_dir,
        Some(InterpString::parse("{{ env.FABRO_WORKDIR }}"))
    );
    assert_eq!(
        settings.model.provider,
        Some(InterpString::parse("anthropic"))
    );
    assert_eq!(settings.model.name, Some(InterpString::parse("sonnet")));
}

mod run_integrations_github_permissions {
    //! Layer + resolver tests for `[run.integrations.github.permissions]`.
    //!
    //! `[run.integrations.github]` uses a hand-rolled `Combine` impl so a
    //! higher layer that sets `permissions = {}` clears the inherited map
    //! ("empty wins as clear"), and an absent block inherits from below.

    use std::collections::HashMap;

    use fabro_types::settings::InterpString;

    use crate::SettingsLayer;
    use crate::layers::Combine;

    fn parse_settings(source: &str) -> SettingsLayer {
        source
            .parse::<SettingsLayer>()
            .expect("fixture should parse via SettingsLayer")
    }

    fn one_perm(key: &str, value: &str) -> HashMap<String, InterpString> {
        HashMap::from([(key.to_string(), InterpString::parse(value))])
    }

    #[test]
    fn workflow_layer_parses_run_level_permissions() {
        let layer = parse_settings(
            r#"
_version = 1

[run.integrations.github.permissions]
issues = "read"
"#,
        );
        let github = layer
            .run
            .as_ref()
            .and_then(|run| run.integrations.as_ref())
            .and_then(|integrations| integrations.github.as_ref())
            .expect("permissions block should be parsed into RunIntegrationsGithubLayer");
        let permissions = github
            .permissions
            .as_ref()
            .expect("permissions table should be present");
        assert_eq!(permissions.len(), 1);
        assert_eq!(
            permissions.get("issues"),
            Some(&InterpString::parse("read"))
        );
    }

    #[test]
    fn workflow_replaces_user_permissions_wholesale() {
        let workflow = parse_settings(
            r#"
_version = 1

[run.integrations.github.permissions]
issues = "write"
"#,
        );
        let user = parse_settings(
            r#"
_version = 1

[run.integrations.github.permissions]
contents = "read"
"#,
        );
        let merged = workflow.combine(user);

        let resolved = super::workflow_settings_from_layer(merged)
            .expect("merged settings should resolve")
            .run;

        assert_eq!(
            resolved.integrations.github.permissions,
            one_perm("issues", "write",)
        );
    }

    #[test]
    fn absent_higher_layer_inherits_lower_permissions() {
        let workflow = parse_settings("_version = 1\n");
        let user = parse_settings(
            r#"
_version = 1

[run.integrations.github.permissions]
contents = "read"
"#,
        );
        let merged = workflow.combine(user);

        let resolved = super::workflow_settings_from_layer(merged)
            .expect("merged settings should resolve")
            .run;

        assert_eq!(
            resolved.integrations.github.permissions,
            one_perm("contents", "read",)
        );
    }

    #[test]
    fn empty_higher_layer_clears_inherited_permissions() {
        // Workflow declares `permissions = {}` -> Some(empty map). The
        // hand-rolled `Combine` keeps Some over fallback, so the resolved
        // map is empty (no token requested) — empty-wins-as-clear.
        let workflow = parse_settings(
            r"
_version = 1

[run.integrations.github]
permissions = {}
",
        );
        let user = parse_settings(
            r#"
_version = 1

[run.integrations.github.permissions]
contents = "read"
"#,
        );
        let merged = workflow.combine(user);

        let resolved = super::workflow_settings_from_layer(merged)
            .expect("merged settings should resolve")
            .run;

        assert!(
            resolved.integrations.github.permissions.is_empty(),
            "empty higher layer should clear inherited permissions, got {:?}",
            resolved.integrations.github.permissions
        );
    }

    #[test]
    fn server_integrations_github_permissions_is_now_unknown_field() {
        let err = r#"
_version = 1

[server.integrations.github.permissions]
issues = "read"
"#
        .parse::<SettingsLayer>()
        .expect_err("stale [server.integrations.github.permissions] must error");
        let message = err.to_string();
        assert!(
            message.contains("permissions") || message.contains("unknown field"),
            "expected unknown-field error mentioning permissions, got: {message}"
        );
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "test asserts the raw template source"
    )]
    #[test]
    fn resolver_preserves_interp_string_in_permissions() {
        let resolved = super::workflow_settings_from_toml(
            r#"
_version = 1

[run.integrations.github.permissions]
issues = "{{ env.GH_PERM_LEVEL }}"
"#,
        )
        .expect("env-token permissions should resolve")
        .run;

        let issues = resolved
            .integrations
            .github
            .permissions
            .get("issues")
            .expect("issues permission should be present");
        // Resolver does NOT eagerly resolve env tokens; the `InterpString`
        // form is preserved for late binding by the consumer.
        assert_eq!(issues.as_source(), "{{ env.GH_PERM_LEVEL }}");
    }
}

mod run_agent_fabro_tools {
    use crate::SettingsLayer;
    use crate::layers::Combine;

    fn parse_settings(source: &str) -> SettingsLayer {
        source
            .parse::<SettingsLayer>()
            .expect("fixture should parse via SettingsLayer")
    }

    #[test]
    fn defaults_to_false_when_run_agent_is_absent() {
        let settings = super::workflow_settings_from_layer(SettingsLayer::default())
            .expect("empty settings should resolve")
            .run;

        assert!(!settings.agent.fabro_tools);
    }

    #[test]
    fn resolves_true_from_run_agent_table() {
        let settings = super::workflow_settings_from_toml(
            r"
_version = 1

[run.agent]
fabro_tools = true
",
        )
        .expect("run.agent.fabro_tools should resolve");

        assert!(settings.run.agent.fabro_tools);
    }

    #[test]
    fn resolves_explicit_false_from_run_agent_table() {
        let settings = super::workflow_settings_from_toml(
            r"
_version = 1

[run.agent]
fabro_tools = false
",
        )
        .expect("run.agent.fabro_tools false should resolve");

        assert!(!settings.run.agent.fabro_tools);
    }

    #[test]
    fn higher_layer_false_overrides_lower_true() {
        let workflow = parse_settings(
            r"
_version = 1

[run.agent]
fabro_tools = false
",
        );
        let user = parse_settings(
            r"
_version = 1

[run.agent]
fabro_tools = true
",
        );
        let merged = workflow.combine(user);

        let settings = super::workflow_settings_from_layer(merged)
            .expect("merged settings should resolve")
            .run;

        assert!(!settings.agent.fabro_tools);
    }
}

mod run_checkpoint_skip_git_hooks {
    //! Layer + resolver tests for `[run.checkpoint] skip_git_hooks`.

    use crate::SettingsLayer;
    use crate::layers::Combine;

    fn parse_settings(source: &str) -> SettingsLayer {
        source
            .parse::<SettingsLayer>()
            .expect("fixture should parse via SettingsLayer")
    }

    #[test]
    fn resolves_skip_git_hooks_true_when_set() {
        let settings = super::workflow_settings_from_toml(
            r"
_version = 1

[run.checkpoint]
skip_git_hooks = true
",
        )
        .expect("settings should resolve")
        .run;

        assert!(settings.checkpoint.skip_git_hooks);
    }

    #[test]
    fn resolves_skip_git_hooks_false_when_omitted() {
        let settings = super::workflow_settings_from_layer(SettingsLayer::default())
            .expect("empty settings should resolve")
            .run;

        assert!(!settings.checkpoint.skip_git_hooks);
    }

    #[test]
    fn higher_layer_false_overrides_lower_layer_true() {
        let workflow = parse_settings(
            r"
_version = 1

[run.checkpoint]
skip_git_hooks = false
",
        );
        let user = parse_settings(
            r"
_version = 1

[run.checkpoint]
skip_git_hooks = true
",
        );
        let merged = workflow.combine(user);

        let settings = super::workflow_settings_from_layer(merged)
            .expect("merged settings should resolve")
            .run;

        assert!(!settings.checkpoint.skip_git_hooks);
    }

    #[test]
    fn exclude_globs_replace_behavior_preserved_when_skip_git_hooks_added() {
        // Higher layer provides skip_git_hooks but no exclude_globs;
        // exclude_globs should still inherit from the lower layer because the
        // higher layer's list is empty.
        let workflow = parse_settings(
            r"
_version = 1

[run.checkpoint]
skip_git_hooks = true
",
        );
        let user = parse_settings(
            r#"
_version = 1

[run.checkpoint]
exclude_globs = ["**/lower/**"]
"#,
        );
        let merged = workflow.combine(user);

        let settings = super::workflow_settings_from_layer(merged)
            .expect("merged settings should resolve")
            .run;

        assert_eq!(settings.checkpoint.exclude_globs, vec!["**/lower/**"]);
        assert!(settings.checkpoint.skip_git_hooks);
    }
}
