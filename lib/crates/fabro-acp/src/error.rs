use crate::command::AcpCommandError;

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
    TimedOut { stderr: String },

    #[error("ACP prompt stopped with {stop_reason}: {text}")]
    StopReason {
        stop_reason: String,
        text:        String,
    },
}

impl From<agent_client_protocol::Error> for AcpError {
    fn from(error: agent_client_protocol::Error) -> Self {
        Self::Protocol(error)
    }
}
