//! The answers workers give to controls: a steer or an interrupt goes out
//! over the control bus with a request id, and the worker acknowledges it
//! over the control stream it arrived on ([`WorkerControlAck`]). The
//! registry pairs each outstanding request with the caller waiting for its
//! answer, for a bounded time; a request nobody answers in time is
//! forgotten, and its caller told the answer is still pending.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use fabro_interview::{WorkerControlAck, WorkerControlOutcome};
use fabro_types::RunId;
use tokio::sync::oneshot;
use tokio::time::timeout;

/// How long a control waits for the worker's answer before the caller is
/// told it is pending.
pub(crate) const WORKER_CONTROL_ACK_WAIT: Duration = Duration::from_secs(5);

/// One outstanding control: the run it went to, and who waits on it.
struct PendingControl {
    run_id: RunId,
    answer: oneshot::Sender<WorkerControlOutcome>,
}

/// The controls awaiting a worker's answer, by request id.
pub(crate) struct WorkerControlAcks {
    pending: Mutex<HashMap<String, PendingControl>>,
    wait:    Duration,
}

/// A registered request: its id, to send with the control, and the answer
/// to wait on.
pub(crate) struct PendingAck {
    pub(crate) request_id: String,
    answer:                oneshot::Receiver<WorkerControlOutcome>,
}

impl WorkerControlAcks {
    /// A registry whose waits last `wait`.
    #[must_use]
    pub(crate) fn new(wait: Duration) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            wait,
        }
    }

    /// Register a control about to go to `run_id`'s worker: the id to send
    /// with it, and the answer to wait on. Registered before the control
    /// is published, so an answer cannot arrive before anyone waits for it.
    pub(crate) fn register(&self, run_id: RunId) -> PendingAck {
        let request_id = ulid::Ulid::new().to_string();
        let (answer, receiver) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(request_id.clone(), PendingControl { run_id, answer });
        PendingAck {
            request_id,
            answer: receiver,
        }
    }

    /// Wait for the answer to `pending`, at most the registry's wait:
    /// `None` when none arrives in time, after which the request is
    /// forgotten and a late answer is dropped.
    pub(crate) async fn wait(&self, pending: PendingAck) -> Option<WorkerControlOutcome> {
        let outcome = timeout(self.wait, pending.answer).await;
        match outcome {
            Ok(Ok(outcome)) => Some(outcome),
            // The sender was dropped: the request was forgotten, or the
            // registry was.
            Ok(Err(_)) => None,
            Err(_elapsed) => {
                self.lock().remove(&pending.request_id);
                None
            }
        }
    }

    /// Answer the request `ack` names, when it is outstanding and went to
    /// `run_id`'s worker: an answer from another run's worker, or to a
    /// request already forgotten, is dropped. Whether an answer was
    /// delivered to a waiting caller.
    pub(crate) fn resolve(&self, run_id: RunId, ack: WorkerControlAck) -> bool {
        let mut pending = self.lock();
        let Some(entry) = pending.get(&ack.request_id) else {
            return false;
        };
        if entry.run_id != run_id {
            return false;
        }
        let Some(entry) = pending.remove(&ack.request_id) else {
            return false;
        };
        drop(pending);
        entry.answer.send(ack.outcome).is_ok()
    }

    /// Forget every request outstanding on `run_id`: its callers are told
    /// the answer is pending at once.
    pub(crate) fn forget_run(&self, run_id: RunId) {
        self.lock().retain(|_, entry| entry.run_id != run_id);
    }

    #[cfg(test)]
    pub(crate) fn outstanding(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, PendingControl>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::fixtures;

    use super::*;

    fn delivered(request_id: &str) -> WorkerControlAck {
        WorkerControlAck::new(request_id, WorkerControlOutcome::Delivered {
            stage: Some("work@1".to_string()),
        })
    }

    #[tokio::test]
    async fn an_answer_reaches_the_caller_waiting_on_its_request() {
        let acks = WorkerControlAcks::new(Duration::from_secs(1));
        let pending = acks.register(fixtures::RUN_1);
        assert!(acks.resolve(fixtures::RUN_1, delivered(&pending.request_id)));
        assert_eq!(
            acks.wait(pending).await,
            Some(WorkerControlOutcome::Delivered {
                stage: Some("work@1".to_string()),
            })
        );
        assert_eq!(acks.outstanding(), 0);
    }

    #[tokio::test]
    async fn a_request_nobody_answers_in_time_is_pending_and_forgotten() {
        let acks = WorkerControlAcks::new(Duration::from_millis(20));
        let pending = acks.register(fixtures::RUN_1);
        let request_id = pending.request_id.clone();
        assert_eq!(acks.wait(pending).await, None);
        assert_eq!(acks.outstanding(), 0);
        assert!(!acks.resolve(fixtures::RUN_1, delivered(&request_id)));
    }

    #[tokio::test]
    async fn another_runs_worker_cannot_answer_a_request() {
        let acks = WorkerControlAcks::new(Duration::from_millis(20));
        let pending = acks.register(fixtures::RUN_1);
        assert!(!acks.resolve(fixtures::RUN_2, delivered(&pending.request_id)));
        assert_eq!(acks.outstanding(), 1);
        assert_eq!(acks.wait(pending).await, None);
    }

    #[tokio::test]
    async fn an_unknown_request_id_is_dropped() {
        let acks = WorkerControlAcks::new(Duration::from_secs(1));
        assert!(!acks.resolve(fixtures::RUN_1, delivered("nobody")));
    }

    #[tokio::test]
    async fn forgetting_a_run_answers_its_callers_with_pending_at_once() {
        let acks = WorkerControlAcks::new(Duration::from_secs(30));
        let pending = acks.register(fixtures::RUN_1);
        let other = acks.register(fixtures::RUN_2);
        acks.forget_run(fixtures::RUN_1);
        assert_eq!(acks.outstanding(), 1);
        assert_eq!(acks.wait(pending).await, None);
        assert!(acks.resolve(fixtures::RUN_2, delivered(&other.request_id)));
    }
}
