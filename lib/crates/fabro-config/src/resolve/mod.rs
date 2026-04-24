mod cli;
mod error;
mod features;
mod project;
mod run;
mod server;
mod workflow;

pub use cli::resolve_cli;
pub use error::ResolveError;
use fabro_types::settings::InterpString;
pub use features::resolve_features;
pub use project::resolve_project;
pub use run::resolve_run;
pub use server::resolve_server;
pub use workflow::resolve_workflow;

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
                path:   path.to_string(),
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

    use crate::{SettingsLayer, WorkflowSettingsBuilder};

    #[test]
    fn resolve_preserves_source_templates_for_mcp_and_hook_strings() {
        let settings = r#"
_version = 1

[server.auth]
methods = ["dev-token"]

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
"#
        .parse::<SettingsLayer>()
        .expect("settings fixture should parse");

        let resolved = WorkflowSettingsBuilder::from_layer(&settings)
            .expect("run settings should resolve")
            .run;
        let mcps = &resolved.agent.mcps;

        assert_eq!(
            mcps.get("stdio").map(|mcp| &mcp.transport),
            Some(&McpTransport::Stdio {
                command: vec!["fabro-mcp".to_string(), "--stdio".to_string()],
                env:     HashMap::from([(
                    "TOKEN".to_string(),
                    "Bearer {{ env.MCP_STDIO_TOKEN }}".to_string(),
                )]),
            })
        );
        assert_eq!(
            mcps.get("http").map(|mcp| &mcp.transport),
            Some(&McpTransport::Http {
                url:     "https://mcp.example.com".to_string(),
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
                port:    3333,
                env:     HashMap::from([(
                    "TOKEN".to_string(),
                    "{{ env.MCP_SANDBOX_TOKEN }}".to_string(),
                )]),
            })
        );

        let hook = resolved
            .hooks
            .iter()
            .find(|hook| hook.name.as_deref() == Some("notify"))
            .expect("notify hook");
        assert_eq!(
            hook.resolved_hook_type().as_deref(),
            Some(&HookType::Http {
                url:              "https://hooks.example.com".to_string(),
                headers:          Some(HashMap::from([(
                    "Authorization".to_string(),
                    "Bearer {{ env.HOOK_TOKEN }}".to_string(),
                )])),
                allowed_env_vars: Vec::new(),
                tls:              TlsMode::Verify,
            })
        );
    }
}
