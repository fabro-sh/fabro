mod combine;
mod defaults;
mod log_filter;
mod resolve_cli;
mod resolve_project;
mod resolve_root;
mod resolve_run;
mod resolve_server;
mod resolve_workflow;

use fabro_types::WorkflowSettings;

use crate::{
    EnvironmentLayer, Error, MergeMap, ResolveErrors, RunLayer, SettingsLayer,
    WorkflowSettingsBuilder,
};

pub(crate) fn seeded_environment_catalog() -> MergeMap<EnvironmentLayer> {
    r#"
[environments.default]
provider = "docker"

[environments.default.image]
docker = "buildpack-deps:noble"

[environments.default.resources]
cpu = 2
memory = "4GB"

[environments.default.lifecycle]
preserve = false
stop_on_terminal = true

[environments.local]
provider = "local"

[environments.docker]
provider = "docker"

[environments.docker.image]
docker = "buildpack-deps:noble"

[environments.docker.resources]
cpu = 2
memory = "4GB"

[environments.docker.lifecycle]
preserve = false
stop_on_terminal = true

[environments.daytona]
provider = "daytona"
"#
    .parse::<SettingsLayer>()
    .expect("seeded environment catalog should parse")
    .environments
}

pub(crate) fn workflow_settings_from_toml(source: &str) -> crate::Result<WorkflowSettings> {
    workflow_settings_from_toml_with_catalog(source, seeded_environment_catalog())
}

pub(crate) fn workflow_settings_from_toml_with_catalog(
    source: &str,
    catalog: MergeMap<EnvironmentLayer>,
) -> crate::Result<WorkflowSettings> {
    WorkflowSettingsBuilder::new()
        .server_manifest_defaults(RunLayer::default(), catalog)
        .workflow_toml(source)?
        .build()
        .map_err(|errors| Error::resolve("failed to resolve workflow settings", errors.into()))
}

pub(crate) fn workflow_settings_from_layer(
    layer: SettingsLayer,
) -> std::result::Result<WorkflowSettings, ResolveErrors> {
    WorkflowSettingsBuilder::new()
        .server_manifest_defaults(RunLayer::default(), seeded_environment_catalog())
        .workflow_layer(layer)
        .build()
}
