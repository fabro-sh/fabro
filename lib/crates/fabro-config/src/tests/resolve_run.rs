use fabro_types::settings::InterpString;
use fabro_types::settings::run::{ApprovalMode, RunGoal, RunMode, WorktreeMode};

use crate::{SettingsLayer, WorkflowSettingsBuilder};

#[test]
fn resolves_run_defaults_from_empty_settings() {
    let settings = WorkflowSettingsBuilder::from_layer(&SettingsLayer::default())
        .expect("empty settings should resolve")
        .run;

    assert_eq!(settings.execution.mode, RunMode::Normal);
    assert_eq!(settings.execution.approval, ApprovalMode::Prompt);
    assert!(settings.execution.retros);
    assert_eq!(settings.prepare.timeout_ms, 300_000);
    assert_eq!(settings.sandbox.provider, "docker");
    assert_eq!(settings.sandbox.local.worktree_mode, WorktreeMode::Clean);
    let docker = settings
        .sandbox
        .docker
        .as_ref()
        .expect("defaults should provide docker settings");
    assert_eq!(docker.image, "buildpack-deps:noble");
    assert_eq!(docker.memory_limit, Some(4_000_000_000));
    assert_eq!(docker.cpu_quota, Some(200_000));
    assert!(!docker.skip_clone);
    assert!(settings.pull_request.is_none());
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
