mod cli;
mod error;
mod features;
mod project;
mod run;
mod server;
mod workflow;

use fabro_types::settings::{
    CliSettings, FeaturesSettings, InterpString, ProjectSettings, RunSettings, ServerSettings,
    Settings, SettingsLayer, WorkflowSettings,
};

pub use cli::resolve_cli;
pub use error::ResolveError;
pub use features::resolve_features;
pub use project::resolve_project;
pub use run::resolve_run;
pub use server::resolve_server;
pub use workflow::resolve_workflow;

pub fn resolve(file: &SettingsLayer) -> Result<Settings, Vec<ResolveError>> {
    let mut errors = Vec::new();
    let project_layer = file.project.clone().unwrap_or_default();
    let workflow_layer = file.workflow.clone().unwrap_or_default();
    let run_layer = file.run.clone().unwrap_or_default();
    let cli_layer = file.cli.clone().unwrap_or_default();
    let server_layer = file.server.clone().unwrap_or_default();
    let features_layer = file.features.clone().unwrap_or_default();

    let settings = Settings {
        project: resolve_project(&project_layer, &mut errors),
        workflow: resolve_workflow(&workflow_layer, &mut errors),
        run: resolve_run(&run_layer, &mut errors),
        cli: resolve_cli(&cli_layer, &mut errors),
        server: resolve_server(&server_layer, &mut errors),
        features: resolve_features(&features_layer, &mut errors),
    };

    if errors.is_empty() {
        Ok(settings)
    } else {
        Err(errors)
    }
}

pub fn resolve_cli_from_file(file: &SettingsLayer) -> Result<CliSettings, Vec<ResolveError>> {
    resolve(file).map(|settings| settings.cli)
}

pub fn resolve_server_from_file(file: &SettingsLayer) -> Result<ServerSettings, Vec<ResolveError>> {
    resolve(file).map(|settings| settings.server)
}

pub fn resolve_project_from_file(
    file: &SettingsLayer,
) -> Result<ProjectSettings, Vec<ResolveError>> {
    resolve(file).map(|settings| settings.project)
}

pub fn resolve_features_from_file(
    file: &SettingsLayer,
) -> Result<FeaturesSettings, Vec<ResolveError>> {
    resolve(file).map(|settings| settings.features)
}

pub fn resolve_run_from_file(file: &SettingsLayer) -> Result<RunSettings, Vec<ResolveError>> {
    resolve(file).map(|settings| settings.run)
}

pub fn resolve_workflow_from_file(
    file: &SettingsLayer,
) -> Result<WorkflowSettings, Vec<ResolveError>> {
    resolve(file).map(|settings| settings.workflow)
}

pub(crate) fn require_interp(
    value: Option<&InterpString>,
    path: &str,
    errors: &mut Vec<ResolveError>,
) -> InterpString {
    value.cloned().unwrap_or_else(|| {
        errors.push(ResolveError::Missing {
            path: path.to_string(),
        });
        InterpString::parse("")
    })
}

pub(crate) fn parse_socket_addr(
    value: &InterpString,
    path: &str,
    errors: &mut Vec<ResolveError>,
) -> std::net::SocketAddr {
    let source = value.as_source();
    match source.parse::<std::net::SocketAddr>() {
        Ok(address) => address,
        Err(err) => {
            errors.push(ResolveError::ParseFailure {
                path: path.to_string(),
                reason: err.to_string(),
            });
            std::net::SocketAddr::from(([127, 0, 0, 1], 0))
        }
    }
}

pub(crate) fn default_interp(path: impl AsRef<std::path::Path>) -> InterpString {
    InterpString::parse(&path.as_ref().to_string_lossy())
}
