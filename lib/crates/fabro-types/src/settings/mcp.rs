use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::combine::Combine;

pub fn default_startup_timeout_secs() -> u64 {
    10
}

pub fn default_tool_timeout_secs() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerSettings {
    pub name: String,
    pub transport: McpTransport,
    #[serde(default = "default_startup_timeout_secs")]
    pub startup_timeout_secs: u64,
    #[serde(default = "default_tool_timeout_secs")]
    pub tool_timeout_secs: u64,
}

impl McpServerSettings {
    #[must_use]
    pub fn startup_timeout(&self) -> Duration {
        Duration::from_secs(self.startup_timeout_secs)
    }

    #[must_use]
    pub fn tool_timeout(&self) -> Duration {
        Duration::from_secs(self.tool_timeout_secs)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        command: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
    /// MCP server that runs inside a sandbox and is accessed via HTTP preview URL.
    /// During session init, the server is started inside the sandbox and this
    /// variant is resolved into an `Http` transport using the sandbox's preview URL.
    Sandbox {
        command: Vec<String>,
        port: u16,
        #[serde(default)]
        env: HashMap<String, String>,
    },
}

impl Combine for McpTransport {
    fn combine(self, _other: Self) -> Self {
        self
    }
}

/// MCP server entry as it appears in TOML config files (without a `name` field).
///
/// Converted to [`McpServerSettings`] via [`McpServerEntry::into_config`].
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct McpServerEntry {
    #[serde(flatten)]
    pub transport: McpTransport,
    #[serde(default = "default_startup_timeout_secs")]
    pub startup_timeout_secs: u64,
    #[serde(default = "default_tool_timeout_secs")]
    pub tool_timeout_secs: u64,
}

impl McpServerEntry {
    pub fn into_config(self, name: String) -> McpServerSettings {
        McpServerSettings {
            name,
            transport: self.transport,
            startup_timeout_secs: self.startup_timeout_secs,
            tool_timeout_secs: self.tool_timeout_secs,
        }
    }
}

impl Combine for McpServerEntry {
    fn combine(self, _other: Self) -> Self {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn stdio_config_construction() {
        let config = McpServerSettings {
            name: "test-server".into(),
            transport: McpTransport::Stdio {
                command: vec![
                    "npx".into(),
                    "-y".into(),
                    "@modelcontextprotocol/server-filesystem".into(),
                ],
                env: HashMap::new(),
            },
            startup_timeout_secs: 10,
            tool_timeout_secs: 60,
        };
        assert_eq!(config.name, "test-server");
        assert_eq!(config.startup_timeout(), Duration::from_secs(10));
        assert_eq!(config.tool_timeout(), Duration::from_secs(60));
    }

    #[test]
    fn http_config_construction() {
        let config = McpServerSettings {
            name: "remote-server".into(),
            transport: McpTransport::Http {
                url: "https://example.com/mcp".into(),
                headers: HashMap::from([("Authorization".into(), "Bearer token".into())]),
            },
            startup_timeout_secs: 30,
            tool_timeout_secs: 60,
        };
        assert_eq!(config.name, "remote-server");
        assert_eq!(config.startup_timeout(), Duration::from_secs(30));
        assert_eq!(config.tool_timeout(), Duration::from_secs(60));
    }

    #[test]
    fn serde_round_trip_stdio() {
        let config = McpServerSettings {
            name: "fs".into(),
            transport: McpTransport::Stdio {
                command: vec!["node".into(), "server.js".into()],
                env: HashMap::from([("NODE_ENV".into(), "production".into())]),
            },
            startup_timeout_secs: 15,
            tool_timeout_secs: 90,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: McpServerSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "fs");
        assert_eq!(deserialized.startup_timeout_secs, 15);
        assert_eq!(deserialized.tool_timeout_secs, 90);
        assert!(
            matches!(deserialized.transport, McpTransport::Stdio { command, .. } if command == vec!["node", "server.js"])
        );
    }

    #[test]
    fn serde_round_trip_http() {
        let config = McpServerSettings {
            name: "remote".into(),
            transport: McpTransport::Http {
                url: "https://mcp.example.com".into(),
                headers: HashMap::new(),
            },
            startup_timeout_secs: 10,
            tool_timeout_secs: 60,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: McpServerSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "remote");
        assert!(
            matches!(deserialized.transport, McpTransport::Http { url, .. } if url == "https://mcp.example.com")
        );
    }

    #[test]
    fn serde_defaults_applied() {
        let json = r#"{"name":"minimal","transport":{"type":"stdio","command":["echo"]}}"#;
        let config: McpServerSettings = serde_json::from_str(json).unwrap();
        assert_eq!(config.startup_timeout_secs, 10);
        assert_eq!(config.tool_timeout_secs, 60);
    }
}
