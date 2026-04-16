mod cli;
mod error;
mod features;
mod project;
mod run;
mod server;
mod workflow;

pub use cli::resolve_cli;
pub use error::ResolveError;
use fabro_types::settings::{
    CliSettings, FeaturesSettings, InterpString, ProjectSettings, RunSettings, ServerSettings,
    Settings, SettingsLayer, WorkflowSettings,
};
pub use features::resolve_features;
pub use project::resolve_project;
pub use run::resolve_run;
pub use server::resolve_server;
pub use workflow::resolve_workflow;

use crate::apply_builtin_defaults;

pub fn resolve(file: &SettingsLayer) -> Result<Settings, Vec<ResolveError>> {
    let file = apply_builtin_defaults(file.clone());
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_types::settings::run::{HookType, McpTransport, TlsMode};

    use super::resolve;
    use crate::parse_settings_layer;

    #[test]
    fn resolve_preserves_source_templates_for_mcp_and_hook_strings() {
        let settings = parse_settings_layer(
            r#"
_version = 1

[run.agent.mcps.stdio]
type = "stdio"
command = ["fabro-mcp", "--stdio"]

[run.agent.mcps.stdio.env]
TOKEN = "Bearer {{ env.MCP_STDIO_TOKEN }}"

[run.agent.mcps.http]
type = "http"
url = "https://mcp.example.com"

[run.agent.mcps.http.headers]
Authorization = "Bearer {{ env.MCP_HTTP_TOKEN }}"

[run.agent.mcps.sandbox]
type = "sandbox"
command = ["fabro-mcp", "--sandbox"]
port = 3333

[run.agent.mcps.sandbox.env]
TOKEN = "{{ env.MCP_SANDBOX_TOKEN }}"

[[run.hooks]]
name = "notify"
event = "run_complete"
url = "https://hooks.example.com"

[run.hooks.headers]
Authorization = "Bearer {{ env.HOOK_TOKEN }}"
"#,
        )
        .expect("settings fixture should parse");

        let resolved = resolve(&settings).expect("settings should resolve");
        let mcps = &resolved.run.agent.mcps;

        assert_eq!(
            mcps.get("stdio").map(|mcp| &mcp.transport),
            Some(&McpTransport::Stdio {
                command: vec!["fabro-mcp".to_string(), "--stdio".to_string()],
                env: HashMap::from([(
                    "TOKEN".to_string(),
                    "Bearer {{ env.MCP_STDIO_TOKEN }}".to_string(),
                )]),
            })
        );
        assert_eq!(
            mcps.get("http").map(|mcp| &mcp.transport),
            Some(&McpTransport::Http {
                url: "https://mcp.example.com".to_string(),
                headers: HashMap::from([(
                    "Authorization".to_string(),
                    "Bearer {{ env.MCP_HTTP_TOKEN }}".to_string(),
                )]),
            })
        );
        assert_eq!(
            mcps.get("sandbox").map(|mcp| &mcp.transport),
            Some(&McpTransport::Sandbox {
                command: vec!["fabro-mcp".to_string(), "--sandbox".to_string()],
                port: 3333,
                env: HashMap::from([(
                    "TOKEN".to_string(),
                    "{{ env.MCP_SANDBOX_TOKEN }}".to_string(),
                )]),
            })
        );

        let hook = resolved
            .run
            .hooks
            .iter()
            .find(|hook| hook.name.as_deref() == Some("notify"))
            .expect("notify hook");
        assert_eq!(
            hook.resolved_hook_type().as_deref(),
            Some(&HookType::Http {
                url: "https://hooks.example.com".to_string(),
                headers: Some(HashMap::from([(
                    "Authorization".to_string(),
                    "Bearer {{ env.HOOK_TOKEN }}".to_string(),
                )])),
                allowed_env_vars: Vec::new(),
                tls: TlsMode::Verify,
            })
        );
    }
}
