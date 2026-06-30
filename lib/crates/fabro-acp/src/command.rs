use std::collections::HashMap;
use std::path::{Path, PathBuf};

use agent_client_protocol::schema::{McpServer, McpServerStdio};
use agent_client_protocol_tokio::AcpAgent;
use fabro_util::shell::shell_join;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpProcessSpec {
    name:    Option<String>,
    program: PathBuf,
    args:    Vec<String>,
    env:     HashMap<String, String>,
}

impl AcpProcessSpec {
    pub fn from_attrs(
        legacy_command: Option<&str>,
        command: Option<&str>,
        config: Option<&str>,
    ) -> Result<Self, AcpCommandError> {
        if legacy_command.is_some() {
            return Err(AcpCommandError::LegacyCommandAttribute);
        }

        match (command, config) {
            (Some(command), None) => Self::from_command_attr(command),
            (None, Some(config)) => Self::from_config_attr(config),
            (None, None) | (Some(_), Some(_)) => Err(AcpCommandError::MissingOverride),
        }
    }

    pub fn from_command_attr(raw: &str) -> Result<Self, AcpCommandError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(AcpCommandError::EmptyOverride);
        }

        let parts = shlex::split(trimmed).ok_or(AcpCommandError::InvalidCommandString)?;
        let agent =
            AcpAgent::from_args(parts).map_err(|_| AcpCommandError::InvalidCommandString)?;
        let mut spec = Self::from_server(agent.into_server())?;
        spec.name = None;
        Ok(spec)
    }

    pub fn from_config_attr(raw: &str) -> Result<Self, AcpCommandError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(AcpCommandError::EmptyOverride);
        }

        let server = parse_config_server(trimmed)?;
        Self::from_server(server)
    }

    fn from_server(server: McpServer) -> Result<Self, AcpCommandError> {
        match server {
            McpServer::Stdio(stdio) => Self::from_stdio_config(stdio),
            _ => Err(AcpCommandError::UnsupportedTransport),
        }
    }

    fn from_stdio_config(stdio: McpServerStdio) -> Result<Self, AcpCommandError> {
        if stdio.command.as_os_str().is_empty() {
            return Err(AcpCommandError::InvalidConfigShape("missing command"));
        }

        let env = stdio
            .env
            .into_iter()
            .map(|env| (env.name, env.value))
            .collect();
        Ok(Self::from_stdio_parts(
            Some(stdio.name),
            stdio.command,
            stdio.args,
            env,
        ))
    }

    fn from_stdio_parts(
        name: Option<String>,
        program: PathBuf,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> Self {
        Self {
            name,
            program,
            args,
            env,
        }
    }

    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

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
    pub fn to_shell_command(&self) -> String {
        render_command(&self.program, &self.args)
    }
}

impl std::fmt::Display for AcpProcessSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_shell_command())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AcpCommandError {
    #[error("acp_command is no longer supported; use acp.command or acp.config")]
    LegacyCommandAttribute,
    #[error("ACP process attribute must not be empty")]
    EmptyOverride,
    #[error("backend=\"acp\" requires exactly one of acp.command or acp.config")]
    MissingOverride,
    #[error("only stdio ACP commands are supported")]
    UnsupportedTransport,
    #[error("failed to parse acp.command as a shell command")]
    InvalidCommandString,
    #[error("failed to parse acp.config as JSON")]
    InvalidConfigJson(#[source] serde_json::Error),
    #[error("invalid acp.config shape: {0}")]
    InvalidConfigShape(&'static str),
}

fn render_command(program: &Path, args: &[String]) -> String {
    shell_join(std::iter::once(program.to_string_lossy().into_owned()).chain(args.iter().cloned()))
}

fn parse_config_server(raw: &str) -> Result<McpServer, AcpCommandError> {
    let mut value: serde_json::Value =
        serde_json::from_str(raw).map_err(AcpCommandError::InvalidConfigJson)?;

    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("stdio") | None => {}
        Some(_) => return Err(AcpCommandError::UnsupportedTransport),
    }

    if let Some(object) = value.as_object_mut() {
        object
            .entry("args".to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        object
            .entry("env".to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    }

    serde_json::from_value(value).map_err(AcpCommandError::InvalidConfigJson)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn command_attr_parses_shell_command() {
        let command = AcpProcessSpec::from_command_attr("python fake_agent.py").unwrap();
        assert_eq!(command.to_string(), "python fake_agent.py");
        assert_eq!(command.name(), None);
        assert_eq!(command.program(), Path::new("python"));
        assert_eq!(command.args(), &["fake_agent.py".to_string()]);
    }

    #[test]
    fn command_attr_parses_leading_env_assignments() {
        let command = AcpProcessSpec::from_command_attr(
            "RUST_LOG=debug TOKEN='secret value' python fake_agent.py",
        )
        .unwrap();

        assert_eq!(command.to_string(), "python fake_agent.py");
        assert_eq!(command.program(), Path::new("python"));
        assert_eq!(
            command.env().get("RUST_LOG").map(String::as_str),
            Some("debug")
        );
        assert_eq!(
            command.env().get("TOKEN").map(String::as_str),
            Some("secret value")
        );
    }

    #[test]
    fn blank_acp_process_attr_is_rejected() {
        let err = AcpProcessSpec::from_command_attr("   ").unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn json_stdio_acp_config_is_supported() {
        let raw = r#"{"type":"stdio","name":"fake","command":"python","args":["fake agent.py"],"env":[{"name":"MODE","value":"test"}]}"#;
        let command = AcpProcessSpec::from_config_attr(raw).unwrap();
        assert_eq!(command.name(), Some("fake"));
        assert_eq!(command.program(), Path::new("python"));
        assert_eq!(command.args(), &["fake agent.py".to_string()]);
        assert_eq!(command.env().get("MODE").map(String::as_str), Some("test"));
    }

    #[test]
    fn json_stdio_acp_config_display_omits_env_contents() {
        let raw = r#"{"type":"stdio","name":"fake","command":"agent","args":["--flag","two words"],"env":[{"name":"OPENAI_API_KEY","value":"secret-key"}]}"#;
        let command = AcpProcessSpec::from_config_attr(raw).unwrap();

        assert_eq!(
            command.env().get("OPENAI_API_KEY").map(String::as_str),
            Some("secret-key")
        );
        assert_eq!(command.to_string(), "agent --flag 'two words'");
        assert!(!command.to_string().contains("secret-key"));
        assert!(!command.to_string().contains("OPENAI_API_KEY"));
    }

    #[test]
    fn non_stdio_acp_config_is_rejected() {
        let raw = r#"{"type":"http","name":"remote","url":"https://example.test/acp"}"#;
        let err = AcpProcessSpec::from_config_attr(raw).unwrap_err();
        assert!(
            err.to_string()
                .contains("only stdio ACP commands are supported")
        );
    }

    #[test]
    fn command_attr_is_always_shell_command_even_when_json_shaped() {
        let command = AcpProcessSpec::from_command_attr(r#"{"type":"stdio"}"#).unwrap();

        assert_ne!(command.program(), Path::new("stdio"));
        assert!(command.args().is_empty());
    }

    #[test]
    fn config_attr_requires_json_stdio_config() {
        let command = AcpProcessSpec::from_config_attr(
            r#"{"type":"stdio","name":"fake","command":"python3","args":["agent.py"]}"#,
        )
        .unwrap();

        assert_eq!(command.name(), Some("fake"));
        assert_eq!(command.program(), Path::new("python3"));
        assert_eq!(command.args(), &["agent.py".to_string()]);

        assert!(AcpProcessSpec::from_config_attr("python3 agent.py").is_err());
        assert!(
            AcpProcessSpec::from_config_attr(
                r#"{"type":"http","name":"remote","url":"https://example.test/acp"}"#
            )
            .is_err()
        );
    }
}
