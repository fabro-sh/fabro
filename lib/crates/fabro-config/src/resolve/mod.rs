mod cli;
mod environment;
mod error;
mod project;
mod run;
mod server;
mod workflow;

pub use cli::resolve_cli;
pub use environment::resolve_environment_layer;
pub(crate) use environment::resolve_run_environment;
pub use error::ResolveError;
use fabro_types::settings::InterpString;
pub use project::resolve_project;
pub use run::resolve_run;
pub use server::resolve_server;
pub use workflow::resolve_workflow;

pub(crate) fn require_interp(
    value: Option<&InterpString>,
    path: &str,
    errors: &mut Vec<ResolveError>,
) -> InterpString {
    require_value(value, path, errors, || InterpString::parse(""))
}

pub(crate) fn require_string(
    value: Option<&String>,
    path: &str,
    errors: &mut Vec<ResolveError>,
) -> String {
    require_value(value, path, errors, String::new)
}

fn require_value<T: Clone>(
    value: Option<&T>,
    path: &str,
    errors: &mut Vec<ResolveError>,
    missing: impl FnOnce() -> T,
) -> T {
    value.cloned().unwrap_or_else(|| {
        errors.push(ResolveError::Missing {
            path: path.to_string(),
        });
        missing()
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "parsed_value special case: the TCP listen address parses the literal source \
              (templates intentionally unsupported)"
)]
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

pub(crate) fn default_string(path: impl AsRef<std::path::Path>) -> String {
    path.as_ref().to_string_lossy().into_owned()
}

/// Warn when a field demoted out of the interpolation set (D2) still contains
/// claimed template tokens. These fields are plain `String` now — `{{ vars.*
/// }}` (which previously substituted via the run-scoped String pass, a
/// now-removed accident) and `{{ env.* }}` are both treated as literal text.
/// Other plain-`String` fields still substitute `{{ vars.* }}` until the
/// String pass itself is retired in a later slice. Unclaimed `{{ ... }}` text
/// (jq programs, Go templates) never interpolated and does not warn.
pub(crate) fn warn_if_demoted_template(field: &str, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };
    if !tracing::enabled!(tracing::Level::WARN) || !value.contains("{{") {
        return;
    }
    if InterpString::parse(value).is_literal() {
        return;
    }
    tracing::warn!(
        field = %field,
        "this field no longer interpolates template tokens and uses the value literally; it \
         was demoted to a plain string in the interpolation unification"
    );
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_types::settings::InterpString;
    use fabro_types::settings::run::{
        HookType, McpHttpProtocol, McpTransport, ResolvedMcpEntry, TlsMode,
    };

    use crate::SettingsLayer;
    use crate::tests::workflow_settings_from_layer;

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

        let resolved = workflow_settings_from_layer(settings)
            .expect("run settings should resolve")
            .run;
        let mcps = &resolved.agent.mcps;
        let transport = |name: &str| {
            mcps.get(name)
                .and_then(ResolvedMcpEntry::as_resolved)
                .map(|server| &server.transport)
        };

        assert_eq!(
            transport("stdio"),
            Some(&McpTransport::Stdio {
                command: vec!["fabro-mcp".to_string(), "--stdio".to_string()],
                env:     HashMap::from([(
                    "TOKEN".to_string(),
                    "Bearer {{ env.MCP_STDIO_TOKEN }}".to_string(),
                )]),
            })
        );
        assert_eq!(
            transport("http"),
            Some(&McpTransport::Http {
                protocol: McpHttpProtocol::default(),
                url:      "https://mcp.example.com".to_string(),
                headers:  HashMap::from([(
                    "Authorization".to_string(),
                    "Bearer {{ env.MCP_HTTP_TOKEN }}".to_string(),
                )]),
            })
        );
        assert_eq!(
            transport("sandbox"),
            Some(&McpTransport::Sandbox {
                protocol: McpHttpProtocol::default(),
                command:  vec!["fabro-mcp".to_string(), "--sandbox".to_string()],
                port:     3333,
                env:      HashMap::from([(
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
                url:              InterpString::parse("https://hooks.example.com"),
                headers:          Some(HashMap::from([(
                    "Authorization".to_string(),
                    InterpString::parse("Bearer {{ env.HOOK_TOKEN }}"),
                )])),
                allowed_env_vars: Vec::new(),
                tls:              TlsMode::Verify,
            })
        );
    }
}
