use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpCommand {
    display: String,
    name:    Option<String>,
    program: PathBuf,
    args:    Vec<String>,
    env:     HashMap<String, String>,
}

impl AcpCommand {
    pub fn from_command_attr(raw: &str) -> Result<Self, AcpCommandError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(AcpCommandError::EmptyOverride);
        }

        let parts = shlex::split(trimmed).ok_or(AcpCommandError::InvalidCommandString)?;
        let Some((program, args)) = parts.split_first() else {
            return Err(AcpCommandError::EmptyOverride);
        };
        let program = PathBuf::from(program);
        let args = args.to_vec();
        let display = render_command(&program, &args);
        Ok(Self {
            display,
            name: None,
            program,
            args,
            env: HashMap::new(),
        })
    }

    pub fn from_config_attr(raw: &str) -> Result<Self, AcpCommandError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(AcpCommandError::EmptyOverride);
        }
        let value: serde_json::Value =
            serde_json::from_str(trimmed).map_err(AcpCommandError::InvalidConfigJson)?;
        reject_non_stdio_json_transport(&value)?;

        let name = value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let program = value
            .get("command")
            .and_then(serde_json::Value::as_str)
            .filter(|command| !command.trim().is_empty())
            .ok_or(AcpCommandError::InvalidConfigShape("missing command"))?;
        let args = string_array_field(&value, "args")?.unwrap_or_default();
        let env = env_array_field(&value)?;

        Ok(Self::from_stdio_parts(
            name,
            PathBuf::from(program),
            args,
            env,
        ))
    }

    fn from_stdio_parts(
        name: Option<String>,
        program: PathBuf,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> Self {
        let display = render_command(&program, &args);
        Self {
            display,
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
    std::iter::once(program.to_string_lossy().into_owned())
        .chain(args.iter().cloned())
        .map(|part| fabro_sandbox::shell_quote(&part))
        .collect::<Vec<_>>()
        .join(" ")
}

fn reject_non_stdio_json_transport(value: &serde_json::Value) -> Result<(), AcpCommandError> {
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("stdio") | None => Ok(()),
        Some(_) => Err(AcpCommandError::UnsupportedTransport),
    }
}

pub type AcpProcessSpec = AcpCommand;

fn string_array_field(
    value: &serde_json::Value,
    key: &'static str,
) -> Result<Option<Vec<String>>, AcpCommandError> {
    let Some(raw) = value.get(key) else {
        return Ok(None);
    };
    let Some(values) = raw.as_array() else {
        return Err(AcpCommandError::InvalidConfigShape(key));
    };
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or(AcpCommandError::InvalidConfigShape(key))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn env_array_field(value: &serde_json::Value) -> Result<HashMap<String, String>, AcpCommandError> {
    let Some(raw) = value.get("env") else {
        return Ok(HashMap::new());
    };
    let Some(values) = raw.as_array() else {
        return Err(AcpCommandError::InvalidConfigShape("env"));
    };

    values
        .iter()
        .map(|value| {
            let name = value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or(AcpCommandError::InvalidConfigShape("env.name"))?;
            let value = value
                .get("value")
                .and_then(serde_json::Value::as_str)
                .ok_or(AcpCommandError::InvalidConfigShape("env.value"))?;
            Ok((name.to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn command_attr_parses_shell_command() {
        let command = AcpProcessSpec::from_command_attr("python fake_agent.py").unwrap();
        assert_eq!(command.to_string(), "python fake_agent.py");
        assert_eq!(command.program(), Path::new("python"));
        assert_eq!(command.args(), &["fake_agent.py".to_string()]);
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
