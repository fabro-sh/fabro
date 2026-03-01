use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransport,
    #[serde(default = "default_startup_timeout_secs")]
    pub startup_timeout_secs: u64,
    #[serde(default = "default_tool_timeout_secs")]
    pub tool_timeout_secs: u64,
}

fn default_startup_timeout_secs() -> u64 {
    10
}

fn default_tool_timeout_secs() -> u64 {
    120
}

impl McpServerConfig {
    #[must_use]
    pub fn startup_timeout(&self) -> Duration {
        Duration::from_secs(self.startup_timeout_secs)
    }

    #[must_use]
    pub fn tool_timeout(&self) -> Duration {
        Duration::from_secs(self.tool_timeout_secs)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_config_construction() {
        let config = McpServerConfig {
            name: "test-server".into(),
            transport: McpTransport::Stdio {
                command: "npx".into(),
                args: vec![
                    "-y".into(),
                    "@modelcontextprotocol/server-filesystem".into(),
                ],
                env: HashMap::new(),
            },
            startup_timeout_secs: 10,
            tool_timeout_secs: 120,
        };
        assert_eq!(config.name, "test-server");
        assert_eq!(config.startup_timeout(), Duration::from_secs(10));
        assert_eq!(config.tool_timeout(), Duration::from_secs(120));
    }

    #[test]
    fn http_config_construction() {
        let config = McpServerConfig {
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
        let config = McpServerConfig {
            name: "fs".into(),
            transport: McpTransport::Stdio {
                command: "node".into(),
                args: vec!["server.js".into()],
                env: HashMap::from([("NODE_ENV".into(), "production".into())]),
            },
            startup_timeout_secs: 15,
            tool_timeout_secs: 90,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "fs");
        assert_eq!(deserialized.startup_timeout_secs, 15);
        assert_eq!(deserialized.tool_timeout_secs, 90);
        assert!(
            matches!(deserialized.transport, McpTransport::Stdio { command, .. } if command == "node")
        );
    }

    #[test]
    fn serde_round_trip_http() {
        let config = McpServerConfig {
            name: "remote".into(),
            transport: McpTransport::Http {
                url: "https://mcp.example.com".into(),
                headers: HashMap::new(),
            },
            startup_timeout_secs: 10,
            tool_timeout_secs: 120,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "remote");
        assert!(
            matches!(deserialized.transport, McpTransport::Http { url, .. } if url == "https://mcp.example.com")
        );
    }

    #[test]
    fn serde_defaults_applied() {
        let json = r#"{"name":"minimal","transport":{"type":"stdio","command":"echo"}}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.startup_timeout_secs, 10);
        assert_eq!(config.tool_timeout_secs, 120);
    }
}
