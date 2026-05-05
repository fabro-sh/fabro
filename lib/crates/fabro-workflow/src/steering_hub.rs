//! Bridge between the worker's HTTP control plane and live agent
//! `Session`s. The hub owns:
//!
//! - A map of currently steerable API-mode sessions, keyed by `StageId` →
//!   active `(session_id, SessionControlHandle)` entries.
//! - A bounded run-wide pending buffer for steers that arrive when no session
//!   is registered (between stages, before the first agent stage, or after a
//!   session ends but before the next registers).
//!
//! Lock discipline (race safety):
//!   - `active` is `std::sync::RwLock`; deliver takes the read lock for the
//!     entire decide-and-push step.
//!   - `pending` is `std::sync::Mutex` taken under the active read lock.
//!   - All methods are sync — no `.await` while holding any lock — so the
//!     `CompletionCoordinator::on_natural_completion` close-the-door dance can
//!     call `detach_if_queue_empty(...)` synchronously from the agent loop.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use fabro_agent::{SessionControlHandle, SteeringItem};
use fabro_types::run_event::AgentSteerDroppedReason;
use fabro_types::{Principal, StageId, SteerKind};

use crate::event::{Emitter, Event};

/// Cap on the steering queue length kept per active session. Overflow
/// evicts the oldest entry (FIFO) and emits `agent.steer.dropped`.
pub const PER_SESSION_QUEUE_CAP: usize = 32;

/// Cap on the run-wide pending buffer used when no session is registered.
/// Overflow evicts the oldest entry (FIFO) and emits `agent.steer.dropped`.
pub const PER_RUN_PENDING_CAP: usize = 32;

#[derive(Debug, Clone)]
struct PendingSteer {
    text:  String,
    actor: Option<Principal>,
}

#[derive(Clone)]
struct ActiveEntry {
    handle:     SessionControlHandle,
    session_id: String,
}

#[allow(
    clippy::module_name_repetitions,
    reason = "external callers refer to it as SteeringHub"
)]
pub struct SteeringHub {
    active:  RwLock<HashMap<StageId, ActiveEntry>>,
    pending: Mutex<VecDeque<PendingSteer>>,
    emitter: Arc<Emitter>,
}

impl SteeringHub {
    #[must_use]
    pub fn new(emitter: Arc<Emitter>) -> Self {
        Self {
            active: RwLock::new(HashMap::new()),
            pending: Mutex::new(VecDeque::new()),
            emitter,
        }
    }

    /// Test-only constructor with an isolated emitter.
    #[cfg(test)]
    #[must_use]
    pub fn for_tests() -> Arc<Self> {
        use fabro_types::RunId;
        Arc::new(Self::new(Arc::new(Emitter::new(RunId::new()))))
    }

    /// Test-only: snapshot of pending buffer length.
    #[cfg(test)]
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.lock().expect("pending lock poisoned").len()
    }

    /// Test-only: snapshot of registered stage count.
    #[cfg(test)]
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active.read().expect("active lock poisoned").len()
    }

    /// Attach an API-mode session as steerable for this stage. Returns
    /// `false` when a different session is already active for the stage.
    pub fn attach_handle(
        &self,
        stage_id: &StageId,
        session_id: &str,
        handle: &SessionControlHandle,
    ) -> bool {
        let mut active = self.active.write().expect("active lock poisoned");
        match active.get_mut(stage_id) {
            Some(entry) if entry.session_id != session_id => false,
            Some(entry) => {
                entry.handle = handle.clone();
                true
            }
            None => {
                active.insert(stage_id.clone(), ActiveEntry {
                    handle:     handle.clone(),
                    session_id: session_id.to_string(),
                });
                true
            }
        }
    }

    /// Drain pending run-wide steers into `handle` as `Append` messages.
    pub fn drain_pending_into(&self, stage_id: &StageId, handle: &SessionControlHandle) {
        let pending: Vec<PendingSteer> = {
            let mut pending = self.pending.lock().expect("pending lock poisoned");
            pending.drain(..).collect()
        };
        for item in pending {
            // Buffered steers always flush as Append — the original
            // Interrupt semantics no longer make sense once the round has
            // rolled over.
            Self::enqueue_into_session_queue(
                handle,
                (item.text, SteerKind::Append, item.actor),
                &self.emitter,
                Some(stage_id),
            );
        }
    }

    /// Detach the session for this stage. Stale session ids are ignored.
    pub fn detach(&self, stage_id: &StageId, session_id: &str) -> bool {
        let mut active = self.active.write().expect("active lock poisoned");
        let Some(entry) = active.get(stage_id) else {
            return false;
        };
        if entry.session_id != session_id {
            return false;
        }
        active.remove(stage_id);
        true
    }

    /// Atomic close-the-door check used by the agent loop's natural-
    /// completion path. Under the `active` write lock: if `handle`'s queue
    /// is empty and the active session id matches, remove the stage and
    /// return `true`. If the queue is non-empty, leave the registration
    /// intact and return `false`.
    pub fn detach_if_queue_empty(
        &self,
        stage_id: &StageId,
        session_id: &str,
        handle: &SessionControlHandle,
    ) -> bool {
        let mut active = self.active.write().expect("active lock poisoned");
        let Some(entry) = active.get(stage_id) else {
            return false;
        };
        if entry.session_id != session_id || !handle.queue_is_empty() {
            return false;
        }
        active.remove(stage_id);
        true
    }

    /// Deliver a steer from the HTTP control plane. Broadcasts to every
    /// active session if any are registered, otherwise parks the message
    /// in the run-wide pending buffer.
    pub fn deliver(&self, text: String, kind: SteerKind, actor: Option<Principal>) {
        // Hold the active read lock for the entire decide-and-dispatch
        // step so register/unregister cannot race with this push.
        let active = self.active.read().expect("active lock poisoned");
        if active.is_empty() {
            let dropped_actor = {
                let mut pending = self.pending.lock().expect("pending lock poisoned");
                let dropped_actor = if pending.len() >= PER_RUN_PENDING_CAP {
                    Some(pending.pop_front().and_then(|d| d.actor))
                } else {
                    None
                };
                pending.push_back(PendingSteer {
                    text,
                    actor: actor.clone(),
                });
                dropped_actor
            };

            if let Some(dropped_actor) = dropped_actor {
                self.emitter.emit(&Event::AgentSteerDropped {
                    reason:  AgentSteerDroppedReason::QueueFull,
                    count:   1,
                    actor:   dropped_actor,
                    node_id: None,
                    visit:   None,
                });
            }
            self.emitter
                .emit(&Event::AgentSteerBuffered { kind, actor });
            drop(active);
            return;
        }

        // Broadcast to every active session.
        for (stage_id, entry) in active.iter() {
            Self::enqueue_into_session_queue(
                &entry.handle,
                (text.clone(), kind, actor.clone()),
                &self.emitter,
                Some(stage_id),
            );
        }
    }

    /// Drain any unconsumed pending steers and emit a single
    /// `agent.steer.dropped` event with `reason: run_ended`. Called from
    /// `operations::start` after the pipeline finishes (success or
    /// failure) but before the emitter is flushed.
    pub fn drain_pending_at_run_end(&self) {
        let count: u32 = {
            let mut pending = self.pending.lock().expect("pending lock poisoned");
            let n = u32::try_from(pending.len()).unwrap_or(u32::MAX);
            pending.clear();
            n
        };
        if count > 0 {
            self.emitter.emit(&Event::AgentSteerDropped {
                reason: AgentSteerDroppedReason::RunEnded,
                count,
                actor: None,
                node_id: None,
                visit: None,
            });
        }
    }

    /// Push an item into a session's queue, evicting the oldest entry and
    /// emitting `agent.steer.dropped { queue_full }` if the cap is hit.
    /// The push + eviction are atomic under the per-session queue lock.
    fn enqueue_into_session_queue(
        handle: &SessionControlHandle,
        item: SteeringItem,
        emitter: &Emitter,
        stage_id: Option<&StageId>,
    ) {
        if let Some((.., evicted_actor)) = handle.enqueue_bounded(item, PER_SESSION_QUEUE_CAP) {
            emitter.emit(&Event::AgentSteerDropped {
                reason:  AgentSteerDroppedReason::QueueFull,
                count:   1,
                actor:   evicted_actor,
                node_id: stage_id.map(|s| s.node_id().to_string()),
                visit:   stage_id.map(StageId::visit),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use fabro_agent::SessionControlHandle;
    use fabro_types::{Principal, StageId, SteerKind, SystemActorKind};

    use super::SteeringHub;

    #[test]
    fn deliver_with_no_active_buffers_message() {
        let hub = SteeringHub::for_tests();
        hub.deliver(
            "hi".into(),
            SteerKind::Append,
            Some(Principal::System {
                system_kind: SystemActorKind::Engine,
            }),
        );
        assert_eq!(hub.pending_len(), 1);
    }

    #[test]
    fn drain_pending_at_run_end_clears_buffer() {
        let hub = SteeringHub::for_tests();
        hub.deliver("a".into(), SteerKind::Append, None);
        hub.deliver("b".into(), SteerKind::Append, None);
        assert_eq!(hub.pending_len(), 2);
        hub.drain_pending_at_run_end();
        assert_eq!(hub.pending_len(), 0);
    }

    #[test]
    fn pending_buffer_evicts_oldest_at_cap() {
        let hub = SteeringHub::for_tests();
        for i in 0..(super::PER_RUN_PENDING_CAP + 5) {
            hub.deliver(format!("msg{i}"), SteerKind::Append, None);
        }
        assert_eq!(hub.pending_len(), super::PER_RUN_PENDING_CAP);
    }

    #[test]
    fn unregister_is_idempotent() {
        let hub = SteeringHub::for_tests();
        let stage = StageId::new("agent-node", 1);
        hub.detach(&stage, "session-a");
        hub.detach(&stage, "session-a");
    }

    #[test]
    fn attach_and_drain_pending_into_first_session() {
        let hub = SteeringHub::for_tests();
        hub.deliver("queued1".into(), SteerKind::Append, None);
        hub.deliver("queued2".into(), SteerKind::Interrupt, None);
        assert_eq!(hub.pending_len(), 2);

        let stage = StageId::new("agent-node", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage, "session-a", &handle));
        hub.drain_pending_into(&stage, &handle);

        assert_eq!(handle.queue_len(), 2);
        assert_eq!(hub.pending_len(), 0);
        assert_eq!(hub.active_count(), 1);
    }

    #[test]
    fn deliver_broadcasts_to_active_sessions() {
        let hub = SteeringHub::for_tests();
        let stage_a = StageId::new("a", 1);
        let stage_b = StageId::new("b", 1);
        let handle_a = SessionControlHandle::new();
        let handle_b = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage_a, "session-a", &handle_a));
        assert!(hub.attach_handle(&stage_b, "session-b", &handle_b));

        hub.deliver("hello".into(), SteerKind::Append, None);

        assert_eq!(handle_a.queue_len(), 1);
        assert_eq!(handle_b.queue_len(), 1);
        assert_eq!(hub.pending_len(), 0);
    }

    #[test]
    fn attach_rejects_different_session_for_same_stage() {
        let hub = SteeringHub::for_tests();
        let stage = StageId::new("a", 1);
        let handle1 = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage, "session-a", &handle1));
        hub.deliver("x".into(), SteerKind::Append, None);
        assert_eq!(handle1.queue_len(), 1);

        let handle2 = SessionControlHandle::new();
        assert!(!hub.attach_handle(&stage, "session-b", &handle2));
        assert_eq!(handle2.queue_len(), 0);
    }

    #[test]
    fn stale_detach_does_not_remove_active_session() {
        let hub = SteeringHub::for_tests();
        let stage = StageId::new("a", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage, "session-a", &handle));

        assert!(!hub.detach(&stage, "session-b"));
        hub.deliver("still-active".into(), SteerKind::Append, None);

        assert_eq!(handle.queue_len(), 1);
        assert_eq!(hub.active_count(), 1);
    }

    #[test]
    fn detach_if_queue_empty_respects_session_id_and_queue_state() {
        let hub = SteeringHub::for_tests();
        let stage = StageId::new("a", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage, "session-a", &handle));

        assert!(!hub.detach_if_queue_empty(&stage, "session-b", &handle));
        hub.deliver("queued".into(), SteerKind::Append, None);
        assert!(!hub.detach_if_queue_empty(&stage, "session-a", &handle));
        assert_eq!(hub.active_count(), 1);
    }

    #[test]
    fn detach_if_queue_empty_removes_matching_empty_session() {
        let hub = SteeringHub::for_tests();
        let stage = StageId::new("a", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage, "session-a", &handle));

        assert!(hub.detach_if_queue_empty(&stage, "session-a", &handle));
        assert_eq!(hub.active_count(), 0);
    }

    #[test]
    fn per_session_queue_evicts_oldest_at_cap() {
        let hub = SteeringHub::for_tests();
        let stage = StageId::new("a", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage, "session-a", &handle));

        for i in 0..(super::PER_SESSION_QUEUE_CAP + 5) {
            hub.deliver(format!("m{i}"), SteerKind::Append, None);
        }
        assert_eq!(handle.queue_len(), super::PER_SESSION_QUEUE_CAP);
    }
}
