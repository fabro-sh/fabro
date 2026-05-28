use std::fmt;

use fabro_interview::WorkerControlEnvelope;
use fabro_types::RunId;
use futures_util::future::BoxFuture;
use tokio::sync::mpsc;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct WorkerControlMessageId(String);

impl WorkerControlMessageId {
    #[must_use]
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WorkerControlMessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("WorkerControlMessageId")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for WorkerControlMessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for WorkerControlMessageId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for WorkerControlMessageId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// Cursor for replaying a run's worker-control stream.
///
/// Future Redis Streams mapping:
/// - `Start` maps to `XREAD ... STREAMS fabro:run:{run_id}:control 0-0`.
/// - `After(id)` maps to `XREAD ... STREAMS fabro:run:{run_id}:control {id}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkerControlCursor {
    Start,
    After(WorkerControlMessageId),
}

impl WorkerControlCursor {
    pub(crate) fn from_after_query(after: Option<&str>) -> Result<Self, WorkerControlBusError> {
        match after {
            None => Ok(Self::Start),
            Some(value) if value.trim().is_empty() => Err(WorkerControlBusError::InvalidCursor {
                cursor: value.to_string(),
                reason: "cursor id must not be empty".to_string(),
            }),
            Some(value) => Ok(Self::After(WorkerControlMessageId::from(value))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerControlDelivery {
    pub(crate) id:       WorkerControlMessageId,
    pub(crate) envelope: WorkerControlEnvelope,
}

#[allow(
    dead_code,
    reason = "The local backend does not construct every cross-backend error variant."
)]
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub(crate) enum WorkerControlBusError {
    #[error("worker control backend is closed")]
    Closed,
    #[error("worker control backend is unavailable")]
    Unavailable,
    #[error("worker control cursor `{cursor}` is invalid: {reason}")]
    InvalidCursor { cursor: String, reason: String },
    #[error("timed out publishing worker control message")]
    PublishTimeout,
}

impl WorkerControlBusError {
    #[must_use]
    pub(crate) fn invalid_cursor(cursor: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidCursor {
            cursor: cursor.into(),
            reason: reason.into(),
        }
    }
}

pub(crate) type WorkerControlReceiver =
    mpsc::Receiver<Result<WorkerControlDelivery, WorkerControlBusError>>;

pub(crate) trait WorkerControlBus: Send + Sync {
    fn publish(
        &self,
        run_id: RunId,
        envelope: WorkerControlEnvelope,
    ) -> BoxFuture<'_, Result<WorkerControlMessageId, WorkerControlBusError>>;

    fn subscribe(
        &self,
        run_id: RunId,
        cursor: WorkerControlCursor,
    ) -> BoxFuture<'_, Result<WorkerControlReceiver, WorkerControlBusError>>;

    fn cleanup_run(&self, run_id: RunId) -> BoxFuture<'_, ()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_id_equality_and_debug_are_opaque() {
        let first = WorkerControlMessageId::from("local:42");
        let same = WorkerControlMessageId::from("local:42");
        let different = WorkerControlMessageId::from("local:43");

        assert_eq!(first, same);
        assert_ne!(first, different);
        assert_eq!(format!("{first}"), "local:42");
        assert_eq!(format!("{first:?}"), "WorkerControlMessageId(\"local:42\")");
    }

    #[test]
    fn absent_after_query_parses_as_start_cursor() {
        assert_eq!(
            WorkerControlCursor::from_after_query(None).unwrap(),
            WorkerControlCursor::Start
        );
    }

    #[test]
    fn present_after_query_parses_as_after_cursor() {
        assert_eq!(
            WorkerControlCursor::from_after_query(Some("local:42")).unwrap(),
            WorkerControlCursor::After(WorkerControlMessageId::from("local:42"))
        );
    }
}
