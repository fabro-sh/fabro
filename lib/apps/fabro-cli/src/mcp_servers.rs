//! Fabro's MCP server settings as the servers pebble starts, for `fabro exec`.
//!
//! Fabro's three transports are pebble's three placements: a `stdio` server
//! is a child of fabro's process, an `http` server is reached directly, and a
//! `sandbox` server is launched in the run sandbox and reached through the
//! sandbox's route to its port. Fabro has always reached a sandbox-hosted SSE
//! server at `/sse` under that route, and still does.

use std::collections::{BTreeMap, HashMap};

use fabro_types::settings::run::{McpHttpProtocol, McpServerSettings, McpTransport};
use pebble_coding_agent::mcp::{McpHttpProtocol as PebbleProtocol, McpPlacement, McpServer};

/// Where a sandbox-hosted SSE server serves its event stream.
const SSE_PATH: &str = "/sse";

/// The pebble server `settings` describes.
#[must_use]
pub(crate) fn pebble_server(settings: &McpServerSettings) -> McpServer {
    let placement = match &settings.transport {
        McpTransport::Stdio { command, env } => McpPlacement::Stdio {
            command:     command.clone(),
            env:         sorted(env),
            current_dir: settings.current_dir.clone(),
            clear_env:   settings.clear_env,
        },
        McpTransport::Http {
            protocol,
            url,
            headers,
        } => McpPlacement::Http {
            url:      url.clone(),
            headers:  sorted(headers),
            protocol: pebble_protocol(*protocol),
        },
        McpTransport::Sandbox {
            protocol,
            command,
            port,
            env,
        } => McpPlacement::Environment {
            command:  command.clone(),
            port:     *port,
            env:      sorted(env),
            protocol: pebble_protocol(*protocol),
            path:     matches!(protocol, McpHttpProtocol::Sse).then(|| SSE_PATH.to_string()),
        },
    };
    McpServer::new(settings.name.clone(), placement)
        .with_startup_timeout(settings.startup_timeout())
        .with_tool_timeout(settings.tool_timeout())
}

/// The pebble servers for every configured server, in configuration order.
pub(crate) fn pebble_servers<'a>(
    settings: impl IntoIterator<Item = &'a McpServerSettings>,
) -> Vec<McpServer> {
    settings.into_iter().map(pebble_server).collect()
}

fn pebble_protocol(protocol: McpHttpProtocol) -> PebbleProtocol {
    match protocol {
        McpHttpProtocol::StreamableHttp => PebbleProtocol::StreamableHttp,
        McpHttpProtocol::Sse => PebbleProtocol::Sse,
    }
}

fn sorted(map: &HashMap<String, String>) -> BTreeMap<String, String> {
    map.iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_stdio_server_keeps_its_directory_and_environment_policy() {
        let settings = McpServerSettings {
            name:                 "echo".into(),
            transport:            McpTransport::Stdio {
                command: vec!["python3".into(), "server.py".into()],
                env:     HashMap::from([("B".into(), "2".into()), ("A".into(), "1".into())]),
            },
            current_dir:          Some(PathBuf::from("/work")),
            clear_env:            true,
            startup_timeout_secs: 7,
            tool_timeout_secs:    9,
        };
        let server = pebble_server(&settings);
        assert_eq!(server.name(), "echo");
        assert_eq!(server.startup_timeout(), Duration::from_secs(7));
        assert_eq!(server.tool_timeout(), Duration::from_secs(9));
        match server.placement() {
            McpPlacement::Stdio {
                command,
                env,
                current_dir,
                clear_env,
            } => {
                assert_eq!(command, &["python3", "server.py"]);
                assert_eq!(
                    env.keys().collect::<Vec<_>>(),
                    ["A", "B"],
                    "a sorted map, so the launch is deterministic"
                );
                assert_eq!(current_dir.as_deref(), Some(std::path::Path::new("/work")));
                assert!(*clear_env);
            }
            other => panic!("expected a stdio placement, got {other:?}"),
        }
    }

    #[test]
    fn an_http_server_keeps_its_protocol_and_headers() {
        let settings = McpServerSettings {
            name: "web".into(),
            transport: McpTransport::Http {
                protocol: McpHttpProtocol::Sse,
                url:      "https://mcp.example/sse".into(),
                headers:  HashMap::from([("authorization".into(), "Bearer x".into())]),
            },
            ..McpServerSettings::default()
        };
        match pebble_server(&settings).placement() {
            McpPlacement::Http {
                url,
                headers,
                protocol,
            } => {
                assert_eq!(url, "https://mcp.example/sse");
                assert_eq!(
                    headers.get("authorization").map(String::as_str),
                    Some("Bearer x")
                );
                assert_eq!(*protocol, PebbleProtocol::Sse);
            }
            other => panic!("expected an http placement, got {other:?}"),
        }
    }

    #[test]
    fn a_sandbox_server_is_an_environment_placement_with_fabros_sse_path() {
        let sse = McpServerSettings {
            name: "playwright".into(),
            transport: McpTransport::Sandbox {
                protocol: McpHttpProtocol::Sse,
                command:  vec!["npx".into(), "@playwright/mcp".into()],
                port:     3100,
                env:      HashMap::new(),
            },
            ..McpServerSettings::default()
        };
        match pebble_server(&sse).placement() {
            McpPlacement::Environment {
                port,
                protocol,
                path,
                ..
            } => {
                assert_eq!(*port, 3100);
                assert_eq!(*protocol, PebbleProtocol::Sse);
                assert_eq!(path.as_deref(), Some("/sse"));
            }
            other => panic!("expected an environment placement, got {other:?}"),
        }
        let streamable = McpServerSettings {
            transport: McpTransport::Sandbox {
                protocol: McpHttpProtocol::StreamableHttp,
                command:  vec!["server".into()],
                port:     3100,
                env:      HashMap::new(),
            },
            ..sse
        };
        match pebble_server(&streamable).placement() {
            McpPlacement::Environment { path, .. } => {
                assert!(
                    path.is_none(),
                    "a streamable server is reached at the route itself"
                );
            }
            other => panic!("expected an environment placement, got {other:?}"),
        }
    }
}
