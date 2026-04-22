//! Helpers for CLI code that manages the local Fabro server on this host.
//!
//! This module is the only generic CLI lifecycle surface allowed to read
//! `[server.*]` settings. User-facing CLI commands outside same-host server
//! lifecycle should not call into it.

use std::path::PathBuf;

use anyhow::Result;
use fabro_server::bind::BindRequest;
use fabro_server::serve::resolve_bind_request_from_settings;
use fabro_types::settings::{ServerAuthMethod, SettingsLayer};

fn render_server_resolve_errors(errors: Vec<fabro_config::ResolveError>) -> anyhow::Error {
    anyhow::anyhow!(
        "failed to resolve server settings:\n{}",
        errors
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    )
}

pub(crate) fn storage_dir(settings: &SettingsLayer) -> Result<PathBuf> {
    let resolved =
        fabro_config::resolve_server_from_file(settings).map_err(render_server_resolve_errors)?;
    let resolved_root = resolved
        .storage
        .root
        .resolve(|name| std::env::var(name).ok())
        .map_err(|err| {
            anyhow::anyhow!(
                "failed to resolve {}: {err}",
                resolved.storage.root.as_source()
            )
        })?;
    Ok(PathBuf::from(resolved_root.value))
}

pub(crate) fn bind_request(
    settings: &SettingsLayer,
    cli_override: Option<&str>,
) -> Result<BindRequest> {
    resolve_bind_request_from_settings(settings, cli_override)
}

pub(crate) fn auth_methods(settings: &SettingsLayer) -> Vec<ServerAuthMethod> {
    fabro_config::resolve_server_from_file(settings)
        .map(|resolved| resolved.auth.methods)
        .unwrap_or_default()
}

pub(crate) fn config_log_level(settings: &SettingsLayer) -> Option<String> {
    settings
        .server
        .as_ref()
        .and_then(|server| server.logging.as_ref())
        .and_then(|logging| logging.level.clone())
}
