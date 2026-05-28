use std::any::{TypeId, type_name};

use fabro_api::types::WorkflowSettings as ApiWorkflowSettings;
use fabro_config::{EnvironmentLayer, MergeMap, RunLayer, SettingsLayer, WorkflowSettingsBuilder};
use fabro_types::WorkflowSettings;

fn seeded_environment_catalog() -> MergeMap<EnvironmentLayer> {
    r#"
[environments.default]
provider = "docker"

[environments.default.image]
docker = "buildpack-deps:noble"
"#
    .parse::<SettingsLayer>()
    .expect("seeded environment catalog should parse")
    .environments
}

fn workflow_settings_from_toml(source: &str) -> WorkflowSettings {
    WorkflowSettingsBuilder::new()
        .server_manifest_defaults(RunLayer::default(), seeded_environment_catalog())
        .workflow_toml(source)
        .expect("workflow settings should parse")
        .build()
        .expect("workflow settings should resolve")
}

#[test]
fn workflow_settings_family_reuses_domain_types() {
    assert_same_type::<ApiWorkflowSettings, WorkflowSettings>();
}

#[test]
fn workflow_settings_json_matches_openapi_shape() {
    let settings = workflow_settings_from_toml(
        r#"
_version = 1

[project]
directory = "workspace"

[workflow]
name = "Ship"
graph = "ship.fabro"

[run]
goal = "Ship it"

[run.execution]
approval = "auto"
"#,
    );

    let json = serde_json::to_value(&settings).expect("workflow settings should serialize");
    assert!(
        json["project"].get("directory").is_none(),
        "resolved project settings should not expose deprecated directory"
    );
    assert_eq!(json["workflow"]["graph"], "ship.fabro");
    assert_eq!(json["run"]["goal"]["type"], "inline");
    assert_eq!(json["run"]["goal"]["value"], "Ship it");
    assert_eq!(json["run"]["execution"]["approval"], "auto");
    assert_eq!(
        json["run"]["environment"]["image"]["docker"],
        "buildpack-deps:noble"
    );
    assert!(json["run"]["environment"]["image"].get("ref").is_none());

    let round_trip: ApiWorkflowSettings =
        serde_json::from_value(json).expect("workflow settings should deserialize");
    assert_eq!(round_trip, settings);
}

#[test]
fn workflow_settings_json_includes_run_checkpoint_skip_git_hooks() {
    let settings = workflow_settings_from_toml(
        r#"
_version = 1

[run.checkpoint]
skip_git_hooks = true
"#,
    );

    let json = serde_json::to_value(&settings).expect("workflow settings should serialize");
    assert_eq!(json["run"]["checkpoint"]["skip_git_hooks"], true);
    assert_eq!(
        json["run"]["checkpoint"]["exclude_globs"],
        serde_json::json!([])
    );

    let round_trip: ApiWorkflowSettings =
        serde_json::from_value(json).expect("workflow settings should deserialize");
    assert_eq!(round_trip, settings);
    assert!(round_trip.run.checkpoint.skip_git_hooks);
}

#[test]
fn workflow_settings_default_run_checkpoint_skip_git_hooks_is_false() {
    let settings = workflow_settings_from_toml("_version = 1\n");
    let json = serde_json::to_value(&settings).expect("workflow settings should serialize");
    assert_eq!(json["run"]["checkpoint"]["skip_git_hooks"], false);
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
