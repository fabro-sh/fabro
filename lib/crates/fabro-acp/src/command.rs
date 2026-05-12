use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use agent_client_protocol::schema::McpServer;
use agent_client_protocol_tokio::AcpAgent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpCommand {
    display: String,
    program: PathBuf,
    args:    Vec<String>,
    env:     HashMap<String, String>,
}

impl AcpCommand {
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    #[must_use]
    pub fn env(&self) -> &HashMap<String, String> {
        &self.env
    }

    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }

    #[must_use]
    pub fn to_shell_command(&self) -> String {
        render_command(&self.program, &self.args)
    }
}

impl std::fmt::Display for AcpCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AcpCommandError {
    #[error("acp_command must not be empty")]
    EmptyOverride,
    #[error(
        "acp_command is required for backend=\"acp\" because Fabro does not install ACP agents"
    )]
    MissingOverride,
    #[error("only stdio ACP commands are supported")]
    UnsupportedTransport,
    #[error("failed to parse acp_command")]
    Parse(#[source] agent_client_protocol::Error),
}

impl From<agent_client_protocol::Error> for AcpCommandError {
    fn from(error: agent_client_protocol::Error) -> Self {
        Self::Parse(error)
    }
}

pub fn resolve_acp_command(override_command: Option<&str>) -> Result<AcpCommand, AcpCommandError> {
    if let Some(raw) = override_command {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(AcpCommandError::EmptyOverride);
        }
        return parse_acp_command(trimmed);
    }

    Err(AcpCommandError::MissingOverride)
}

fn parse_acp_command(raw: &str) -> Result<AcpCommand, AcpCommandError> {
    reject_non_stdio_json_transport(raw)?;

    let agent = AcpAgent::from_str(raw)?;
    let McpServer::Stdio(stdio) = agent.into_server() else {
        return Err(AcpCommandError::UnsupportedTransport);
    };

    let program = stdio.command;
    let args = stdio.args;
    let display = render_command(&program, &args);

    Ok(AcpCommand {
        display,
        program,
        args,
        env: stdio
            .env
            .into_iter()
            .map(|env| (env.name, env.value))
            .collect(),
    })
}

fn render_command(program: &Path, args: &[String]) -> String {
    std::iter::once(program.to_string_lossy().into_owned())
        .chain(args.iter().cloned())
        .map(|part| fabro_sandbox::shell_quote(&part))
        .collect::<Vec<_>>()
        .join(" ")
}

fn reject_non_stdio_json_transport(raw: &str) -> Result<(), AcpCommandError> {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with('{') {
        return Ok(());
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Ok(());
    };

    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("stdio") | None => Ok(()),
        Some(_) => Err(AcpCommandError::UnsupportedTransport),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn missing_acp_command_is_rejected() {
        let err = resolve_acp_command(None).unwrap_err();
        assert!(
            err.to_string()
                .contains("acp_command is required for backend=\"acp\"")
        );
    }

    #[test]
    fn explicit_acp_command_overrides_provider_default() {
        let command = resolve_acp_command(Some("python fake_agent.py")).unwrap();
        assert_eq!(command.to_string(), "python fake_agent.py");
        assert_eq!(command.program(), Path::new("python"));
        assert_eq!(command.args(), &["fake_agent.py".to_string()]);
    }

    #[test]
    fn blank_acp_command_is_rejected() {
        let err = resolve_acp_command(Some("   ")).unwrap_err();
        assert!(err.to_string().contains("acp_command must not be empty"));
    }

    #[test]
    fn json_stdio_acp_command_is_supported() {
        let raw = r#"{"type":"stdio","name":"fake","command":"python","args":["fake agent.py"],"env":[{"name":"MODE","value":"test"}]}"#;
        let command = resolve_acp_command(Some(raw)).unwrap();
        assert_eq!(command.program(), Path::new("python"));
        assert_eq!(command.args(), &["fake agent.py".to_string()]);
        assert_eq!(command.env().get("MODE").map(String::as_str), Some("test"));
    }

    #[test]
    fn json_stdio_acp_command_display_omits_env_contents() {
        let raw = r#"{"type":"stdio","name":"fake","command":"agent","args":["--flag","two words"],"env":[{"name":"OPENAI_API_KEY","value":"secret-key"}]}"#;
        let command = resolve_acp_command(Some(raw)).unwrap();

        assert_eq!(
            command.env().get("OPENAI_API_KEY").map(String::as_str),
            Some("secret-key")
        );
        assert_eq!(command.to_string(), "agent --flag 'two words'");
        assert!(!command.to_string().contains("secret-key"));
        assert!(!command.to_string().contains("OPENAI_API_KEY"));
    }

    #[test]
    fn non_stdio_acp_command_is_rejected() {
        let raw = r#"{"type":"http","name":"remote","url":"https://example.test/acp"}"#;
        let err = resolve_acp_command(Some(raw)).unwrap_err();
        assert!(
            err.to_string()
                .contains("only stdio ACP commands are supported")
        );
    }
}
