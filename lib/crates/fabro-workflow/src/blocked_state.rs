use std::sync::Mutex;

use fabro_types::BlockedReason;

use crate::event::{Emitter, Event};

/// Tracks the number of unresolved human interview questions for a run.
///
/// Emits `run.blocked` on the `0 -> 1` transition and `run.unblocked` on
/// the `1 -> 0` transition. Thread-safe for parallel human stages.
pub struct BlockedStateTracker {
    state: Mutex<BlockedState>,
    emitter: std::sync::Arc<Emitter>,
}

struct BlockedState {
    unresolved_count: usize,
}

impl BlockedStateTracker {
    pub fn new(emitter: std::sync::Arc<Emitter>) -> Self {
        Self {
            state: Mutex::new(BlockedState {
                unresolved_count: 0,
            }),
            emitter,
        }
    }

    /// Called when a new interview question is started. If this is the first
    /// unresolved question (0 -> 1), emits `run.blocked`.
    pub fn on_interview_started(&self) {
        let mut state = self.state.lock().expect("blocked state lock poisoned");
        let was_zero = state.unresolved_count == 0;
        state.unresolved_count += 1;
        if was_zero {
            drop(state);
            self.emitter.emit(&Event::RunBlocked {
                blocked_reason: BlockedReason::HumanInputRequired,
            });
        }
    }

    /// Called when an interview question is resolved (completed, timed out, or
    /// interrupted). If this was the last unresolved question (1 -> 0), emits
    /// `run.unblocked`.
    pub fn on_interview_resolved(&self) {
        let mut state = self.state.lock().expect("blocked state lock poisoned");
        state.unresolved_count = state.unresolved_count.saturating_sub(1);
        let now_zero = state.unresolved_count == 0;
        drop(state);
        if now_zero {
            self.emitter.emit(&Event::RunUnblocked);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fabro_types::RunId;

    use super::*;

    fn test_emitter() -> Arc<Emitter> {
        let run_id = RunId::new();
        Arc::new(Emitter::new(run_id))
    }

    #[test]
    fn first_interview_emits_blocked() {
        let emitter = test_emitter();
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        emitter.on_event(move |event| {
            events_clone
                .lock()
                .unwrap()
                .push(event.body.event_name().to_string());
        });
        let tracker = BlockedStateTracker::new(emitter);
        tracker.on_interview_started();
        let names: Vec<String> = events.lock().unwrap().clone();
        assert!(names.contains(&"run.blocked".to_string()));
    }

    #[test]
    fn second_interview_does_not_emit_blocked() {
        let emitter = test_emitter();
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        emitter.on_event(move |event| {
            events_clone
                .lock()
                .unwrap()
                .push(event.body.event_name().to_string());
        });
        let tracker = BlockedStateTracker::new(emitter);
        tracker.on_interview_started();
        tracker.on_interview_started();
        let names: Vec<String> = events.lock().unwrap().clone();
        assert_eq!(
            names.iter().filter(|n| *n == "run.blocked").count(),
            1,
            "should emit run.blocked exactly once"
        );
    }

    #[test]
    fn last_resolution_emits_unblocked() {
        let emitter = test_emitter();
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        emitter.on_event(move |event| {
            events_clone
                .lock()
                .unwrap()
                .push(event.body.event_name().to_string());
        });
        let tracker = BlockedStateTracker::new(emitter);
        tracker.on_interview_started();
        tracker.on_interview_started();
        tracker.on_interview_resolved(); // 2 -> 1, no unblocked
        let names: Vec<String> = events.lock().unwrap().clone();
        assert!(
            !names.contains(&"run.unblocked".to_string()),
            "should not emit run.unblocked with 1 remaining"
        );
        tracker.on_interview_resolved(); // 1 -> 0, unblocked
        let names: Vec<String> = events.lock().unwrap().clone();
        assert!(
            names.contains(&"run.unblocked".to_string()),
            "should emit run.unblocked on last resolution"
        );
    }

    #[test]
    fn exactly_one_blocked_and_one_unblocked_for_single_interview() {
        let emitter = test_emitter();
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        emitter.on_event(move |event| {
            events_clone
                .lock()
                .unwrap()
                .push(event.body.event_name().to_string());
        });
        let tracker = BlockedStateTracker::new(emitter);
        tracker.on_interview_started();
        tracker.on_interview_resolved();
        let names: Vec<String> = events.lock().unwrap().clone();
        assert_eq!(names.iter().filter(|n| *n == "run.blocked").count(), 1);
        assert_eq!(names.iter().filter(|n| *n == "run.unblocked").count(), 1);
    }
}
