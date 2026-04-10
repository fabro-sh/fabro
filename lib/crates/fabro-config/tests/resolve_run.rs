use fabro_config::ConfigLayer;
use fabro_types::settings::run::{ApprovalMode, RunGoal, RunMode, WorktreeMode};
use fabro_types::settings::{InterpString, SettingsFile};

fn parse(source: &str) -> SettingsFile {
    ConfigLayer::parse(source)
        .expect("fixture should parse")
        .into()
}

#[test]
fn resolves_run_defaults_from_empty_settings() {
    let settings = fabro_config::resolve_run_from_file(&SettingsFile::default())
        .expect("empty settings should resolve");

    assert_eq!(settings.execution.mode, RunMode::Normal);
    assert_eq!(settings.execution.approval, ApprovalMode::Prompt);
    assert!(settings.execution.retros);
    assert_eq!(settings.prepare.timeout_ms, 300_000);
    assert_eq!(settings.sandbox.provider, "local");
    assert_eq!(settings.sandbox.local.worktree_mode, WorktreeMode::Clean);
    assert!(settings.pull_request.is_none());
}

#[test]
fn preserves_goal_variants_and_model_sources() {
    let file = parse(
        r#"
_version = 1

[run]
working_dir = "${env.FABRO_WORKDIR}"

[run.goal]
file = "${env.GOAL_FILE}"

[run.model]
provider = "anthropic"
name = "sonnet"
"#,
    );

    let settings = fabro_config::resolve_run_from_file(&file).expect("run settings should resolve");

    match settings.goal {
        Some(RunGoal::File(path)) => {
            assert_eq!(path, InterpString::parse("${env.GOAL_FILE}"));
        }
        other => panic!("expected file goal, got {other:?}"),
    }
    assert_eq!(
        settings.working_dir,
        Some(InterpString::parse("${env.FABRO_WORKDIR}"))
    );
    assert_eq!(
        settings.model.provider,
        Some(InterpString::parse("anthropic"))
    );
    assert_eq!(settings.model.name, Some(InterpString::parse("sonnet")));
}
