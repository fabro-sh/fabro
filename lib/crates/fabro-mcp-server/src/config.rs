#![expect(
    clippy::disallowed_methods,
    reason = "MCP client config setup intentionally performs small synchronous JSON file reads/writes from a CLI command."
)]

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use serde_json::map::Entry;
use serde_json::{Map, Value, json};

use crate::{McpAgent, McpConfigSettings, McpInitSettings};

const SERVER_NAME: &str = "fabro";

pub fn config_json(settings: &McpConfigSettings) -> Result<String> {
    serde_json::to_string_pretty(&generic_config(settings))
        .map(|json| format!("{json}\n"))
        .context("failed to render Fabro MCP client config")
}

pub fn init_agent(settings: &McpInitSettings) -> Result<()> {
    let entry = server_entry(&settings.config);
    for path in agent_config_paths(settings.agent, &settings.home_dir) {
        merge_server_entry(&path, entry.clone())?;
    }
    Ok(())
}

fn generic_config(settings: &McpConfigSettings) -> Value {
    json!({
        "mcpServers": {
            SERVER_NAME: server_entry(settings)
        }
    })
}

fn server_entry(settings: &McpConfigSettings) -> Value {
    json!({
        "command": "fabro",
        "args": start_args(settings),
    })
}

fn start_args(settings: &McpConfigSettings) -> Vec<String> {
    let mut args = vec!["mcp".to_string(), "start".to_string()];
    if let Some(server) = settings.server.as_ref() {
        args.push("--server".to_string());
        args.push(server.clone());
    }
    if let Some(storage_dir) = settings.storage_dir.as_deref() {
        args.push("--storage-dir".to_string());
        args.push(storage_dir.display().to_string());
    }
    args
}

fn merge_server_entry(path: &Path, entry: Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut root = match std::fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str::<Value>(&contents)
            .with_context(|| format!("failed to parse MCP config {}", path.display()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
    };

    let root_object = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("MCP config {} must contain a JSON object", path.display()))?;

    let servers = match root_object.entry("mcpServers") {
        Entry::Vacant(entry) => entry.insert(Value::Object(Map::new())),
        Entry::Occupied(entry) => entry.into_mut(),
    };
    let servers_object = servers.as_object_mut().ok_or_else(|| {
        anyhow!(
            "MCP config {} field mcpServers must contain a JSON object",
            path.display()
        )
    })?;
    servers_object.insert(SERVER_NAME.to_string(), entry);

    let rendered = serde_json::to_string_pretty(&root)
        .map(|json| format!("{json}\n"))
        .with_context(|| format!("failed to render MCP config {}", path.display()))?;
    std::fs::write(path, rendered).with_context(|| format!("failed to write {}", path.display()))
}

fn agent_config_paths(agent: McpAgent, home_dir: &Path) -> Vec<PathBuf> {
    match agent {
        McpAgent::Claude => vec![
            claude_desktop_config_path(home_dir),
            claude_code_config_path(home_dir),
        ],
        McpAgent::Cursor => vec![home_dir.join(".cursor").join("mcp.json")],
        McpAgent::Windsurf => vec![
            home_dir
                .join(".codeium")
                .join("windsurf")
                .join("mcp_config.json"),
        ],
    }
}

fn claude_code_config_path(home_dir: &Path) -> PathBuf {
    home_dir.join(".claude.json")
}

fn claude_desktop_config_path(home_dir: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home_dir
            .join("Library")
            .join("Application Support")
            .join("Claude")
            .join("claude_desktop_config.json")
    }

    #[cfg(target_os = "linux")]
    {
        home_dir
            .join(".config")
            .join("Claude")
            .join("claude_desktop_config.json")
    }

    #[cfg(target_os = "windows")]
    {
        let app_data = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir.join("AppData").join("Roaming"));
        app_data.join("Claude").join("claude_desktop_config.json")
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        home_dir
            .join(".config")
            .join("Claude")
            .join("claude_desktop_config.json")
    }
}
