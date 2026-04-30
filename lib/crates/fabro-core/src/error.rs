use std::fmt;

use crate::outcome::{FailureCategory, FailureDetail, Outcome, OutcomeMeta, StageOutcome};

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

/// Structured failure data on handler errors. Maps to workflow error's
/// is_retryable(), failure_class(), failure_signature_hint(),
/// to_fail_outcome().
#[derive(Debug, Clone)]
pub struct HandlerErrorDetail {
    pub message:   String,
    pub retryable: bool,
    pub category:  Option<FailureCategory>,
    pub signature: Option<String>,
}

impl fmt::Display for HandlerErrorDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
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
        node_id:      String,
        visits:       usize,
        limit:        usize,
        limit_source: VisitLimitSource,
    },
    #[error("stall timeout on node \"{node_id}\"")]
    StallTimeout { node_id: String },
    #[error("{detail}")]
    Handler { detail: HandlerErrorDetail },
    #[error("{0}")]
    Other(String),
}

impl Error {
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
                status: StageOutcome::Failed {
                    retry_requested: false,
                },
                failure: Some(FailureDetail {
                    message:   detail.message.clone(),
                    category:  detail.category.unwrap_or(FailureCategory::Deterministic),
                    signature: detail.signature.clone(),
                }),
                ..Outcome::default()
            },
            other => Outcome::fail(&other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_error_display() {
        assert_eq!(
            Error::NodeNotFound { id: "n1".into() }.to_string(),
            "node not found: n1"
        );
        assert_eq!(
            Error::NoStartNode.to_string(),
            "no start node found in graph"
        );
        assert_eq!(Error::Cancelled.to_string(), "run cancelled");
        assert_eq!(
            Error::Blocked {
                message: "hook denied".into(),
            }
            .to_string(),
            "blocked: hook denied"
        );
        assert_eq!(
            Error::VisitLimitExceeded {
                node_id:      "n1".into(),
                visits:       5,
                limit:        3,
                limit_source: VisitLimitSource::Node,
            }
            .to_string(),
            "node \"n1\" visited 5 times (node limit 3); run is stuck in a cycle"
        );
        assert_eq!(
            Error::StallTimeout {
                node_id: "work".into(),
            }
            .to_string(),
            "stall timeout on node \"work\""
        );
        assert_eq!(
            Error::Other("something broke".into()).to_string(),
            "something broke"
        );
    }

    #[test]
    fn core_error_handler_is_retryable() {
        let retryable = Error::handler(HandlerErrorDetail {
            message:   "timeout".into(),
            retryable: true,
            category:  None,
            signature: None,
        });
        assert!(retryable.is_retryable());

        let not_retryable = Error::handler(HandlerErrorDetail {
            message:   "bad input".into(),
            retryable: false,
            category:  None,
            signature: None,
        });
        assert!(!not_retryable.is_retryable());
    }

    #[test]
    fn core_error_handler_to_fail_outcome() {
        use crate::outcome::FailureCategory;
        let err = Error::handler(HandlerErrorDetail {
            message:   "api down".into(),
            retryable: true,
            category:  Some(FailureCategory::TransientInfra),
            signature: Some("sig123".into()),
        });
        let outcome: Outcome = err.to_fail_outcome();
        assert_eq!(outcome.status, StageOutcome::Failed {
            retry_requested: false,
        });
        let failure = outcome.failure.unwrap();
        assert_eq!(failure.message, "api down");
        assert_eq!(failure.category, FailureCategory::TransientInfra);
        assert_eq!(failure.signature.as_deref(), Some("sig123"));
    }

    #[test]
    fn core_error_non_handler_not_retryable() {
        assert!(!Error::NodeNotFound { id: "x".into() }.is_retryable());
        assert!(!Error::Cancelled.is_retryable());
        assert!(!Error::NoStartNode.is_retryable());
        assert!(
            !Error::Blocked {
                message: "no".into(),
            }
            .is_retryable()
        );
        assert!(!Error::Other("err".into()).is_retryable());
    }
}
