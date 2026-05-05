use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use ::fabro_types::{ExecOutputTail, RunEvent, RunId, RunNoticeCode, RunNoticeLevel};
use chrono::Utc;
use fabro_agent::{WorktreeEvent, WorktreeEventCallback};

use super::Event;
use super::convert::to_run_event_at;
use crate::stage_scope::StageScope;

fn epoch_millis() -> i64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

/// Listener callback type for workflow run events.
type EventListener = Arc<dyn Fn(&RunEvent) + Send + Sync>;

/// Callback-based event emitter for workflow run events.
pub struct Emitter {
    run_id:        RunId,
    listeners:     std::sync::Mutex<Vec<EventListener>>,
    /// Epoch milliseconds of the last `emit()` or `touch()` call. 0 until first
    /// event.
    last_event_at: AtomicI64,
}

impl std::fmt::Debug for Emitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.listeners.lock().map_or(0, |l| l.len());
        f.debug_struct("Emitter")
            .field("run_id", &self.run_id)
            .field("listener_count", &count)
            .field("last_event_at", &self.last_event_at.load(Ordering::Relaxed))
            .finish()
    }
}

impl Default for Emitter {
    fn default() -> Self {
        Self::new(RunId::new())
    }
}

impl Emitter {
    #[must_use]
    pub fn new(run_id: RunId) -> Self {
        Self {
            run_id,
            listeners: std::sync::Mutex::new(Vec::new()),
            last_event_at: AtomicI64::new(0),
        }
    }

    #[must_use]
    pub fn run_id(&self) -> RunId {
        self.run_id
    }

    pub fn on_event(&self, listener: impl Fn(&RunEvent) + Send + Sync + 'static) {
        self.listeners
            .lock()
            .expect("listeners lock poisoned")
            .push(Arc::new(listener));
    }

    pub fn emit(&self, event: &Event) {
        self.emit_with_scope(event, None);
    }

    pub fn emit_scoped(&self, event: &Event, scope: &StageScope) {
        self.emit_with_scope(event, Some(scope));
    }

    pub fn notice(&self, level: RunNoticeLevel, code: RunNoticeCode, message: impl Into<String>) {
        self.emit(&Event::RunNotice {
            level,
            code: code.to_string(),
            message: message.into(),
            exec_output_tail: None,
        });
    }

    pub fn notice_with_tail(
        &self,
        level: RunNoticeLevel,
        code: RunNoticeCode,
        message: impl Into<String>,
        exec_output_tail: Option<ExecOutputTail>,
    ) {
        self.emit(&Event::RunNotice {
            level,
            code: code.to_string(),
            message: message.into(),
            exec_output_tail,
        });
    }

    fn emit_with_scope(&self, event: &Event, scope: Option<&StageScope>) {
        self.last_event_at.store(epoch_millis(), Ordering::Relaxed);
        event.trace();
        if let Event::WorkflowRunStarted { run_id, .. } = event {
            debug_assert_eq!(
                *run_id, self.run_id,
                "workflow run started event must match emitter run_id"
            );
        }
        let stored = to_run_event_at(&self.run_id, event, Utc::now(), scope);
        self.dispatch_run_event(&stored);
    }

    pub(crate) fn dispatch_run_event(&self, event: &RunEvent) {
        self.last_event_at.store(epoch_millis(), Ordering::Relaxed);
        // Clone the listener list so we don't hold the lock during dispatch.
        // This prevents deadlocks if a listener calls emit() reentrantly.
        // Note: listeners added during this emit() won't receive the current event.
        let snapshot: Vec<EventListener> = self
            .listeners
            .lock()
            .expect("listeners lock poisoned")
            .clone();
        for listener in &snapshot {
            listener(event);
        }
    }

    /// Returns the epoch milliseconds of the last `emit()` or `touch()` call.
    /// Returns 0 if neither has been called.
    pub fn last_event_at(&self) -> i64 {
        self.last_event_at.load(Ordering::Relaxed)
    }

    /// Manually update the last-event timestamp (e.g. to seed the watchdog at
    /// workflow run start).
    pub fn touch(&self) {
        self.last_event_at.store(epoch_millis(), Ordering::Relaxed);
    }

    /// Build a [`WorktreeEventCallback`] that forwards worktree lifecycle
    /// events as [`Event`]s on this emitter.
    pub fn worktree_callback(self: Arc<Self>) -> WorktreeEventCallback {
        Arc::new(move |event| match event {
            WorktreeEvent::BranchCreated { branch, sha } => {
                self.emit(&Event::GitBranch { branch, sha });
            }
            WorktreeEvent::WorktreeAdded { path, branch } => {
                self.emit(&Event::GitWorktreeAdd { path, branch });
            }
            WorktreeEvent::WorktreeRemoved { path } => {
                self.emit(&Event::GitWorktreeRemove { path });
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ::fabro_types::fixtures;

    use super::*;
    use crate::event::Event;

    #[test]
    fn event_emitter_new_has_no_listeners() {
        let emitter = Emitter::new(fixtures::RUN_1);
        assert_eq!(emitter.listeners.lock().unwrap().len(), 0);
    }

    #[test]
    fn event_emitter_calls_listener_with_envelope() {
        let emitter = Emitter::new(fixtures::RUN_1);
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        emitter.on_event(move |event| {
            received_clone.lock().unwrap().push(event.clone());
        });
        emitter.emit(&Event::WorkflowRunStarted {
            name:         "test".to_string(),
            run_id:       fixtures::RUN_1,
            base_branch:  None,
            base_sha:     None,
            run_branch:   None,
            worktree_dir: None,
            goal:         None,
        });
        let events = received.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_name(), "run.started");
        assert_eq!(events[0].run_id, fixtures::RUN_1);
        assert!(events[0].id.len() >= 32);
    }

    #[test]
    fn event_emitter_default() {
        let emitter = Emitter::default();
        assert_eq!(emitter.listeners.lock().unwrap().len(), 0);
    }
}
