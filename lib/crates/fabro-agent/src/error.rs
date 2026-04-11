use fabro_llm::Error as LlmError;

/// Why a session was interrupted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptReason {
    WallClockTimeout,
    Cancelled,
}

impl std::fmt::Display for InterruptReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WallClockTimeout => write!(f, "wall clock timeout"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Error {
    #[error("LLM error: {0}")]
    Llm(#[from] LlmError),

    #[error("Session is closed")]
    SessionClosed,

    #[error("Invalid state: {0}")]
    InvalidState(String),

    #[error("Tool execution error: {0}")]
    ToolExecution(String),

    #[error("Interrupted: {0}")]
    Interrupted(InterruptReason),
}

pub type Result<T> = std::result::Result<T, Error>;
pub type AgentError = Error;

#[cfg(test)]
mod tests {
    use fabro_llm::{ProviderErrorDetail, ProviderErrorKind};

    use super::*;

    #[test]
    fn agent_error_from_sdk_error() {
        let sdk_err = LlmError::Network {
            message: "connection refused".into(),
            source:  None,
        };
        let agent_err = Error::from(sdk_err);
        assert!(matches!(agent_err, Error::Llm(_)));
        assert!(agent_err.to_string().contains("connection refused"));
    }

    #[test]
    fn session_closed_display() {
        let err = Error::SessionClosed;
        assert_eq!(err.to_string(), "Session is closed");
    }

    #[test]
    fn invalid_state_display() {
        let err = Error::InvalidState("bad state".into());
        assert_eq!(err.to_string(), "Invalid state: bad state");
    }

    #[test]
    fn tool_execution_display() {
        let err = Error::ToolExecution("command failed".into());
        assert_eq!(err.to_string(), "Tool execution error: command failed");
    }

    #[test]
    fn interrupted_display() {
        let err = Error::Interrupted(InterruptReason::Cancelled);
        assert_eq!(err.to_string(), "Interrupted: cancelled");
    }

    #[test]
    fn interrupted_wall_clock_timeout_display() {
        let err = Error::Interrupted(InterruptReason::WallClockTimeout);
        assert_eq!(err.to_string(), "Interrupted: wall clock timeout");
    }

    // --- Serde roundtrip tests ---

    #[test]
    fn serde_roundtrip_llm_network() {
        let err = Error::Llm(LlmError::Network {
            message: "connection refused".into(),
            source:  None,
        });
        let json = serde_json::to_string(&err).unwrap();
        let deserialized: Error = serde_json::from_str(&json).unwrap();
        assert_eq!(err.to_string(), deserialized.to_string());
    }

    #[test]
    fn serde_roundtrip_llm_provider() {
        let err = Error::Llm(LlmError::Provider {
            kind:   ProviderErrorKind::RateLimit,
            detail: Box::new(ProviderErrorDetail {
                message:     "too fast".into(),
                provider:    "openai".into(),
                status_code: Some(429),
                error_code:  None,
                retry_after: Some(2.0),
                raw:         None,
            }),
        });
        let json = serde_json::to_string(&err).unwrap();
        let deserialized: Error = serde_json::from_str(&json).unwrap();
        assert_eq!(err.to_string(), deserialized.to_string());
    }

    #[test]
    fn serde_roundtrip_session_closed() {
        let err = Error::SessionClosed;
        let json = serde_json::to_string(&err).unwrap();
        let deserialized: Error = serde_json::from_str(&json).unwrap();
        assert_eq!(err.to_string(), deserialized.to_string());
    }

    #[test]
    fn serde_roundtrip_invalid_state() {
        let err = Error::InvalidState("bad".into());
        let json = serde_json::to_string(&err).unwrap();
        let deserialized: Error = serde_json::from_str(&json).unwrap();
        assert_eq!(err.to_string(), deserialized.to_string());
    }

    #[test]
    fn serde_roundtrip_tool_execution() {
        let err = Error::ToolExecution("cmd failed".into());
        let json = serde_json::to_string(&err).unwrap();
        let deserialized: Error = serde_json::from_str(&json).unwrap();
        assert_eq!(err.to_string(), deserialized.to_string());
    }

    #[test]
    fn serde_roundtrip_interrupted() {
        let err = Error::Interrupted(InterruptReason::Cancelled);
        let json = serde_json::to_string(&err).unwrap();
        let deserialized: Error = serde_json::from_str(&json).unwrap();
        assert_eq!(err.to_string(), deserialized.to_string());
    }

    // --- Clone tests ---

    #[test]
    fn clone_all_variants() {
        let errors: Vec<Error> = vec![
            Error::Llm(LlmError::Network {
                message: "refused".into(),
                source:  None,
            }),
            Error::SessionClosed,
            Error::InvalidState("reason".into()),
            Error::ToolExecution("reason".into()),
            Error::Interrupted(InterruptReason::Cancelled),
        ];
        for err in &errors {
            assert_eq!(err.to_string(), err.clone().to_string());
        }
    }

    // --- Serde tag format tests ---

    #[test]
    fn serde_tag_format_llm() {
        let err = Error::Llm(LlmError::Network {
            message: "refused".into(),
            source:  None,
        });
        let json = serde_json::to_string(&err).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "llm");
    }

    #[test]
    fn serde_tag_format_session_closed() {
        let err = Error::SessionClosed;
        let json = serde_json::to_string(&err).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "session_closed");
    }

    #[test]
    fn serde_tag_format_invalid_state() {
        let err = Error::InvalidState("x".into());
        let json = serde_json::to_string(&err).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "invalid_state");
    }

    #[test]
    fn serde_tag_format_tool_execution() {
        let err = Error::ToolExecution("x".into());
        let json = serde_json::to_string(&err).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "tool_execution");
    }

    #[test]
    fn serde_tag_format_interrupted() {
        let err = Error::Interrupted(InterruptReason::WallClockTimeout);
        let json = serde_json::to_string(&err).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "interrupted");
        assert_eq!(v["data"], "wall_clock_timeout");
    }
}
