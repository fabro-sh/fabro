use fabro_types::settings::InterpString;
use fabro_types::settings::run::{ApprovalMode, RunGoal, RunMode};

use crate::{SettingsLayer, WorkflowSettingsBuilder};

#[test]
fn run_model_controls_round_trip_through_resolve() {
    let settings = WorkflowSettingsBuilder::from_toml(
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
    let settings = WorkflowSettingsBuilder::from_layer(&SettingsLayer::default())
        .expect("empty settings should resolve")
        .run;

    assert!(settings.model.controls.reasoning_effort.is_none());
    assert!(settings.model.controls.speed.is_none());
}

#[test]
fn resolves_run_defaults_from_empty_settings() {
    let settings = WorkflowSettingsBuilder::from_layer(&SettingsLayer::default())
        .expect("empty settings should resolve")
        .run;

    assert_eq!(settings.execution.mode, RunMode::Normal);
    assert_eq!(settings.execution.approval, ApprovalMode::Prompt);
    assert_eq!(settings.prepare.timeout_ms, 300_000);
    assert_eq!(settings.sandbox.provider, "docker");
    assert!(settings.sandbox.stop_on_terminal);
    let docker = settings
        .sandbox
        .docker
        .as_ref()
        .expect("defaults should provide docker settings");
    assert_eq!(docker.image, "buildpack-deps:noble");
    assert_eq!(docker.memory_limit, Some(4_000_000_000));
    assert_eq!(docker.cpu_quota, Some(200_000));
    assert!(settings.clone.enabled);
    assert!(settings.run_branch.enabled);
    assert!(settings.run_branch.push);
    assert!(settings.meta_branch.enabled);
    assert!(settings.meta_branch.push);
    assert!(settings.pull_request.is_none());
}

#[test]
fn resolves_run_level_clone_branch_controls() {
    let settings = WorkflowSettingsBuilder::from_toml(
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
    let settings = WorkflowSettingsBuilder::from_toml(
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
    let disabled_branch = WorkflowSettingsBuilder::from_toml(
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

    let disabled_push = WorkflowSettingsBuilder::from_toml(
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
fn provider_skip_clone_is_rejected() {
    let err = r"
_version = 1

[run.sandbox.docker]
skip_clone = true
"
    .parse::<SettingsLayer>()
    .expect_err("provider-level skip_clone should be unknown");
    let message = err.to_string();
    assert!(
        message.contains("skip_clone") || message.contains("unknown field"),
        "expected unknown-field error mentioning skip_clone, got: {message}"
    );
}

#[test]
fn resolved_run_chat_surfaces_are_slack_only() {
    let settings = WorkflowSettingsBuilder::from_toml(
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
fn resolves_explicit_stop_on_terminal_false() {
    let settings = WorkflowSettingsBuilder::from_toml(
        r"
_version = 1

[run.sandbox]
stop_on_terminal = false
",
    )
    .expect("sandbox stop_on_terminal setting should resolve")
    .run;

    assert!(!settings.sandbox.stop_on_terminal);
}

#[test]
fn resolves_minimal_local_provider_without_docker_table() {
    let settings = WorkflowSettingsBuilder::from_toml(
        r#"
_version = 1

[run.sandbox]
provider = "local"
"#,
    )
    .expect("minimal local sandbox settings should resolve")
    .run;

    assert_eq!(settings.sandbox.provider, "local");
    assert!(settings.sandbox.docker.is_some());
}

#[test]
fn preserves_goal_variants_and_model_sources() {
    let settings = WorkflowSettingsBuilder::from_toml(
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

    use crate::layers::Combine;
    use crate::{SettingsLayer, WorkflowSettingsBuilder};

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

        let resolved = WorkflowSettingsBuilder::from_layer(&merged)
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

        let resolved = WorkflowSettingsBuilder::from_layer(&merged)
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

        let resolved = WorkflowSettingsBuilder::from_layer(&merged)
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

    #[test]
    fn resolver_preserves_interp_string_in_permissions() {
        let resolved = WorkflowSettingsBuilder::from_toml(
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
