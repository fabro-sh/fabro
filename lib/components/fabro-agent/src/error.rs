use fabro_llm::Error as LlmError;

/// Why a session was interrupted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptReason {
    WallClockTimeout,
    Cancelled,
    /// The run's token/cost budget was exhausted (`[run.limits]`).
    BudgetExhausted,
}

impl std::fmt::Display for InterruptReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WallClockTimeout => write!(f, "wall clock timeout"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::BudgetExhausted => write!(f, "run budget exhausted"),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum CompactionError {
    #[error("summary request failed: {0}")]
    Llm(#[source] LlmError),

    #[error(
        "generated summary was empty after trimming; refused to replace \
         {summarized_turn_count} turns and left history intact"
    )]
    EmptySummary { summarized_turn_count: usize },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Error {
    #[error("LLM error: {0}")]
    Llm(#[from] LlmError),

    #[error("Context compaction failed: {0}")]
    Compaction(#[from] CompactionError),

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

#[cfg(test)]
mod tests {
    use fabro_llm::{ProviderErrorDetail, ProviderErrorKind};
    use fabro_util::error;

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
    fn compaction_error_preserves_llm_source_chain() {
        let err = Error::Compaction(CompactionError::Llm(LlmError::Network {
            message: "connection refused".into(),
            source:  None,
        }));

        let chain = error::collect_chain(&err);

        assert!(
            chain.len() >= 3,
            "expected agent, compaction, and LLM errors in the source chain: {chain:?}"
        );
        assert!(
            chain
                .last()
                .is_some_and(|cause| cause.contains("connection refused")),
            "underlying LLM failure missing from source chain: {chain:?}"
        );
    }

    #[test]
    fn empty_compaction_summary_display() {
        let err = Error::Compaction(CompactionError::EmptySummary {
            summarized_turn_count: 3,
        });
        assert_eq!(
            err.to_string(),
            "Context compaction failed: generated summary was empty after trimming; \
             refused to replace 3 turns and left history intact"
        );
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
    fn serde_roundtrip_compaction() {
        let err = Error::Compaction(CompactionError::EmptySummary {
            summarized_turn_count: 3,
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
            Error::Compaction(CompactionError::EmptySummary {
                summarized_turn_count: 3,
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
    fn serde_tag_format_compaction() {
        let err = Error::Compaction(CompactionError::EmptySummary {
            summarized_turn_count: 3,
        });
        let json = serde_json::to_string(&err).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "compaction");
        assert_eq!(v["data"]["type"], "empty_summary");
        assert_eq!(v["data"]["data"]["summarized_turn_count"], 3);
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
