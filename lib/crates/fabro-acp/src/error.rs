use fabro_types::{CommandTermination, ExecOutputTail};

use crate::command::AcpCommandError;

#[derive(Debug)]
pub struct AcpProcessExit {
    pub termination:      CommandTermination,
    pub exit_code:        Option<i32>,
    pub exec_output_tail: Option<ExecOutputTail>,
}

impl std::fmt::Display for AcpProcessExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let exit_code = self
            .exit_code
            .map_or_else(|| "unknown".to_string(), |code| code.to_string());
        write!(
            f,
            "ACP process exited before protocol completed: termination={}, exit_code={exit_code}",
            self.termination
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    #[error(transparent)]
    Command(#[from] AcpCommandError),

    #[error(transparent)]
    Sandbox(#[from] fabro_sandbox::Error),

    #[error("ACP protocol error")]
    Protocol(#[source] agent_client_protocol::Error),

    #[error("ACP turn was cancelled")]
    Cancelled,

    #[error("ACP turn timed out")]
    TimedOut {
        exec_output_tail: Option<ExecOutputTail>,
    },

    #[error("{0}")]
    ProcessExited(AcpProcessExit),

    #[error("ACP prompt stopped with {stop_reason}: {text}")]
    StopReason {
        stop_reason: String,
        text:        String,
    },
}

impl AcpError {
    #[must_use]
    pub fn exec_output_tail(&self) -> Option<ExecOutputTail> {
        match self {
            Self::TimedOut { exec_output_tail } => exec_output_tail.clone(),
            Self::ProcessExited(exit) => exit.exec_output_tail.clone(),
            Self::Sandbox(source) => source.default_redacted_output_tail(),
            _ => None,
        }
    }
}

impl From<agent_client_protocol::Error> for AcpError {
    fn from(error: agent_client_protocol::Error) -> Self {
        Self::Protocol(error)
    }
}
