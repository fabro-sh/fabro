mod cli;
mod error;
mod features;
mod project;
mod run;
mod server;
mod workflow;

use fabro_types::settings::{
    CliSettings, FeaturesSettings, ProjectSettings, RunSettings, ServerSettings, SettingsFile,
    WorkflowSettings,
};

pub use cli::resolve_cli;
pub use error::ResolveError;
pub use features::resolve_features;
pub use project::resolve_project;
pub use run::resolve_run;
pub use server::resolve_server;
pub use workflow::resolve_workflow;

pub fn resolve_cli_from_file(file: &SettingsFile) -> Result<CliSettings, Vec<ResolveError>> {
    let mut errors = Vec::new();
    let layer = file.cli.as_ref().cloned().unwrap_or_default();
    let resolved = resolve_cli(&layer, &mut errors);
    if errors.is_empty() {
        Ok(resolved)
    } else {
        Err(errors)
    }
}

pub fn resolve_server_from_file(file: &SettingsFile) -> Result<ServerSettings, Vec<ResolveError>> {
    let mut errors = Vec::new();
    let layer = file.server.as_ref().cloned().unwrap_or_default();
    let resolved = resolve_server(&layer, &mut errors);
    if errors.is_empty() {
        Ok(resolved)
    } else {
        Err(errors)
    }
}

pub fn resolve_project_from_file(
    file: &SettingsFile,
) -> Result<ProjectSettings, Vec<ResolveError>> {
    let mut errors = Vec::new();
    let layer = file.project.as_ref().cloned().unwrap_or_default();
    let resolved = resolve_project(&layer, &mut errors);
    if errors.is_empty() {
        Ok(resolved)
    } else {
        Err(errors)
    }
}

pub fn resolve_features_from_file(
    file: &SettingsFile,
) -> Result<FeaturesSettings, Vec<ResolveError>> {
    let mut errors = Vec::new();
    let layer = file.features.as_ref().cloned().unwrap_or_default();
    let resolved = resolve_features(&layer, &mut errors);
    if errors.is_empty() {
        Ok(resolved)
    } else {
        Err(errors)
    }
}

pub fn resolve_run_from_file(file: &SettingsFile) -> Result<RunSettings, Vec<ResolveError>> {
    let mut errors = Vec::new();
    let layer = file.run.as_ref().cloned().unwrap_or_default();
    let resolved = resolve_run(&layer, &mut errors);
    if errors.is_empty() {
        Ok(resolved)
    } else {
        Err(errors)
    }
}

pub fn resolve_workflow_from_file(
    file: &SettingsFile,
) -> Result<WorkflowSettings, Vec<ResolveError>> {
    let mut errors = Vec::new();
    let layer = file.workflow.as_ref().cloned().unwrap_or_default();
    let resolved = resolve_workflow(&layer, &mut errors);
    if errors.is_empty() {
        Ok(resolved)
    } else {
        Err(errors)
    }
}

pub(crate) fn require_interp(
    value: Option<&fabro_types::settings::InterpString>,
    path: &str,
    errors: &mut Vec<ResolveError>,
) -> fabro_types::settings::InterpString {
    value.cloned().unwrap_or_else(|| {
        errors.push(ResolveError::Missing {
            path: path.to_string(),
        });
        fabro_types::settings::InterpString::parse("")
    })
}

pub(crate) fn parse_socket_addr(
    value: &fabro_types::settings::InterpString,
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

pub(crate) fn default_interp(
    path: impl AsRef<std::path::Path>,
) -> fabro_types::settings::InterpString {
    fabro_types::settings::InterpString::parse(&path.as_ref().to_string_lossy())
}
