use std::fmt;

use crate::outcome::{FailureCategory, FailureDetail, Outcome, OutcomeMeta, StageStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitLimitSource {
    Node,
    Graph,
}

impl fmt::Display for VisitLimitSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node => write!(f, "node"),
            Self::Graph => write!(f, "graph"),
        }
    }
}

/// Structured failure data on handler errors. Maps to FabroError's
/// is_retryable(), failure_class(), failure_signature_hint(), to_fail_outcome().
#[derive(Debug, Clone)]
pub struct HandlerErrorDetail {
    pub message: String,
    pub retryable: bool,
    pub category: Option<FailureCategory>,
    pub signature: Option<String>,
}

impl fmt::Display for HandlerErrorDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("node not found: {id}")]
    NodeNotFound { id: String },
    #[error("no start node found in graph")]
    NoStartNode,
    #[error("run cancelled")]
    Cancelled,
    #[error("blocked: {message}")]
    Blocked { message: String },
    #[error(
        "node \"{node_id}\" visited {visits} times ({limit_source} limit {limit}); run is stuck in a cycle"
    )]
    VisitLimitExceeded {
        node_id: String,
        visits: usize,
        limit: usize,
        limit_source: VisitLimitSource,
    },
    #[error("stall timeout on node \"{node_id}\"")]
    StallTimeout { node_id: String },
    #[error("{detail}")]
    Handler { detail: HandlerErrorDetail },
    #[error("{0}")]
    Other(String),
}

impl CoreError {
    pub fn handler(detail: HandlerErrorDetail) -> Self {
        Self::Handler { detail }
    }

    pub fn blocked(message: impl Into<String>) -> Self {
        Self::Blocked {
            message: message.into(),
        }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Handler { detail } if detail.retryable)
    }

    pub fn to_fail_outcome<M: OutcomeMeta>(&self) -> Outcome<M> {
        match self {
            Self::Handler { detail } => Outcome {
                status: StageStatus::Fail,
                failure: Some(FailureDetail {
                    message: detail.message.clone(),
                    category: detail.category.unwrap_or(FailureCategory::Deterministic),
                    signature: detail.signature.clone(),
                }),
                ..Outcome::default()
            },
            other => Outcome::fail(&other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_error_display() {
        assert_eq!(
            CoreError::NodeNotFound { id: "n1".into() }.to_string(),
            "node not found: n1"
        );
        assert_eq!(
            CoreError::NoStartNode.to_string(),
            "no start node found in graph"
        );
        assert_eq!(CoreError::Cancelled.to_string(), "run cancelled");
        assert_eq!(
            CoreError::Blocked {
                message: "hook denied".into()
            }
            .to_string(),
            "blocked: hook denied"
        );
        assert_eq!(
            CoreError::VisitLimitExceeded {
                node_id: "n1".into(),
                visits: 5,
                limit: 3,
                limit_source: VisitLimitSource::Node,
            }
            .to_string(),
            "node \"n1\" visited 5 times (node limit 3); run is stuck in a cycle"
        );
        assert_eq!(
            CoreError::StallTimeout {
                node_id: "work".into()
            }
            .to_string(),
            "stall timeout on node \"work\""
        );
        assert_eq!(
            CoreError::Other("something broke".into()).to_string(),
            "something broke"
        );
    }

    #[test]
    fn core_error_handler_is_retryable() {
        let retryable = CoreError::handler(HandlerErrorDetail {
            message: "timeout".into(),
            retryable: true,
            category: None,
            signature: None,
        });
        assert!(retryable.is_retryable());

        let not_retryable = CoreError::handler(HandlerErrorDetail {
            message: "bad input".into(),
            retryable: false,
            category: None,
            signature: None,
        });
        assert!(!not_retryable.is_retryable());
    }

    #[test]
    fn core_error_handler_to_fail_outcome() {
        use crate::outcome::FailureCategory;
        let err = CoreError::handler(HandlerErrorDetail {
            message: "api down".into(),
            retryable: true,
            category: Some(FailureCategory::TransientInfra),
            signature: Some("sig123".into()),
        });
        let outcome: Outcome = err.to_fail_outcome();
        assert_eq!(outcome.status, StageStatus::Fail);
        let failure = outcome.failure.unwrap();
        assert_eq!(failure.message, "api down");
        assert_eq!(failure.category, FailureCategory::TransientInfra);
        assert_eq!(failure.signature.as_deref(), Some("sig123"));
    }

    #[test]
    fn core_error_non_handler_not_retryable() {
        assert!(!CoreError::NodeNotFound { id: "x".into() }.is_retryable());
        assert!(!CoreError::Cancelled.is_retryable());
        assert!(!CoreError::NoStartNode.is_retryable());
        assert!(
            !CoreError::Blocked {
                message: "no".into()
            }
            .is_retryable()
        );
        assert!(!CoreError::Other("err".into()).is_retryable());
    }
}
