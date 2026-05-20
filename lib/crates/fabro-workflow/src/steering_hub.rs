//! Bridge between the worker's HTTP control plane and live agent
//! `Session`s. The hub owns:
//!
//! - A map of currently steerable live sessions, keyed by `StageId` → active
//!   `(session_id, ActiveControlHandle)` entries.
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
//!     call `detach_if_no_pending_control_work(...)` synchronously from the
//!     agent loop.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use fabro_agent::SessionControlHandle;
use fabro_types::run_event::AgentSteerDroppedReason;
use fabro_types::{Principal, StageId, SteeringMessage};

use crate::event::{Emitter, Event};

/// Cap on the steering queue length kept per active session. Overflow
/// evicts the oldest entry (FIFO) and emits `agent.steer.dropped`.
pub const PER_SESSION_QUEUE_CAP: usize = 32;

/// Cap on the run-wide pending buffer used when no session is registered.
/// Overflow evicts the oldest entry (FIFO) and emits `agent.steer.dropped`.
pub const PER_RUN_PENDING_CAP: usize = 32;

pub trait ActiveControlHandle: Send + Sync {
    fn enqueue_bounded(&self, item: SteeringMessage, cap: usize) -> Option<SteeringMessage>;
    fn interrupt(&self, actor: Option<Principal>);
    fn interrupt_then_enqueue_bounded(
        &self,
        item: SteeringMessage,
        cap: usize,
    ) -> Option<SteeringMessage>;
    fn has_pending_control_work(&self) -> bool;
}

impl ActiveControlHandle for SessionControlHandle {
    fn enqueue_bounded(&self, item: SteeringMessage, cap: usize) -> Option<SteeringMessage> {
        Self::enqueue_bounded(self, item, cap)
    }

    fn interrupt(&self, actor: Option<Principal>) {
        Self::interrupt(self, actor);
    }

    fn interrupt_then_enqueue_bounded(
        &self,
        item: SteeringMessage,
        cap: usize,
    ) -> Option<SteeringMessage> {
        Self::interrupt_then_enqueue_bounded(self, item, cap)
    }

    fn has_pending_control_work(&self) -> bool {
        Self::has_pending_control_work(self)
    }
}

#[derive(Clone)]
struct ActiveEntry {
    handle:     Arc<dyn ActiveControlHandle>,
    session_id: String,
}

#[allow(
    clippy::module_name_repetitions,
    reason = "external callers refer to it as SteeringHub"
)]
pub struct SteeringHub {
    active:  RwLock<HashMap<StageId, ActiveEntry>>,
    pending: Mutex<VecDeque<SteeringMessage>>,
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

    /// Attach a live backend session as steerable for this stage. Returns
    /// `false` when a different session is already active for the stage.
    pub fn attach_handle(
        &self,
        stage_id: &StageId,
        session_id: &str,
        handle: Arc<dyn ActiveControlHandle>,
    ) -> bool {
        let mut active = self.active.write().expect("active lock poisoned");
        match active.get_mut(stage_id) {
            Some(entry) if entry.session_id != session_id => false,
            Some(entry) => {
                entry.handle = handle;
                true
            }
            None => {
                active.insert(stage_id.clone(), ActiveEntry {
                    handle,
                    session_id: session_id.to_string(),
                });
                true
            }
        }
    }

    /// Drain pending run-wide steers into `handle`.
    pub fn drain_pending_into(&self, stage_id: &StageId, handle: &dyn ActiveControlHandle) {
        let pending: Vec<SteeringMessage> = {
            let mut pending = self.pending.lock().expect("pending lock poisoned");
            pending.drain(..).collect()
        };
        for item in pending {
            Self::enqueue_into_session_queue(handle, item, &self.emitter, Some(stage_id));
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
    pub fn detach_if_no_pending_control_work(
        &self,
        stage_id: &StageId,
        session_id: &str,
        handle: &dyn ActiveControlHandle,
    ) -> bool {
        let mut active = self.active.write().expect("active lock poisoned");
        let Some(entry) = active.get(stage_id) else {
            return false;
        };
        if entry.session_id != session_id || handle.has_pending_control_work() {
            return false;
        }
        active.remove(stage_id);
        true
    }

    /// Deliver a steer from the HTTP control plane. Broadcasts to every
    /// active session if any are registered, otherwise parks the message
    /// in the run-wide pending buffer.
    pub fn deliver_steer(&self, text: String, actor: Option<Principal>) {
        self.emitter.emit(&Event::RunSteer {
            text:  text.clone(),
            actor: actor.clone(),
        });

        // Hold the active read lock for the entire decide-and-dispatch
        // step so register/unregister cannot race with this push.
        let active = self.active.read().expect("active lock poisoned");
        if active.is_empty() {
            let dropped_actor = {
                let mut pending = self.pending.lock().expect("pending lock poisoned");
                let dropped_actor = if pending.len() >= PER_RUN_PENDING_CAP {
                    pending.pop_front().and_then(|d| d.actor)
                } else {
                    None
                };
                pending.push_back(SteeringMessage::new(text, actor.clone()));
                dropped_actor
            };

            if let Some(dropped_actor) = dropped_actor {
                self.emitter.emit(&Event::AgentSteerDropped {
                    reason:  AgentSteerDroppedReason::QueueFull,
                    count:   1,
                    actor:   Some(dropped_actor),
                    node_id: None,
                    visit:   None,
                });
            }
            self.emitter.emit(&Event::AgentSteerBuffered { actor });
            drop(active);
            return;
        }

        // Broadcast to every active session.
        for (stage_id, entry) in active.iter() {
            Self::enqueue_into_session_queue(
                entry.handle.as_ref(),
                SteeringMessage::new(text.clone(), actor.clone()),
                &self.emitter,
                Some(stage_id),
            );
        }
    }

    /// Interrupt every active steerable session. Does not buffer when no
    /// active session exists.
    pub fn interrupt(&self, actor: Option<&Principal>) {
        let active = self.active.read().expect("active lock poisoned");
        if active.is_empty() {
            return;
        }

        self.emitter.emit(&Event::RunInterrupt {
            actor: actor.cloned(),
        });
        for (stage_id, entry) in active.iter() {
            entry.handle.interrupt(actor.cloned());
            self.emitter.emit(&Event::AgentInterruptInjected {
                node_id:    stage_id.node_id().to_string(),
                visit:      stage_id.visit(),
                session_id: entry.session_id.clone(),
                actor:      actor.cloned(),
            });
        }
    }

    /// Atomically apply interrupt semantics, then deliver steering text to
    /// every active steerable session. Emits persisted run events in the same
    /// order.
    pub fn interrupt_then_steer(&self, text: &str, actor: Option<&Principal>) {
        let active = self.active.read().expect("active lock poisoned");
        if active.is_empty() {
            return;
        }

        self.emitter.emit(&Event::RunInterrupt {
            actor: actor.cloned(),
        });
        self.emitter.emit(&Event::RunSteer {
            text:  text.to_string(),
            actor: actor.cloned(),
        });

        for (stage_id, entry) in active.iter() {
            if let Some(evicted) = entry.handle.interrupt_then_enqueue_bounded(
                SteeringMessage::new(text, actor.cloned()),
                PER_SESSION_QUEUE_CAP,
            ) {
                self.emitter.emit(&Event::AgentSteerDropped {
                    reason:  AgentSteerDroppedReason::QueueFull,
                    count:   1,
                    actor:   evicted.actor,
                    node_id: Some(stage_id.node_id().to_string()),
                    visit:   Some(stage_id.visit()),
                });
            }
            self.emitter.emit(&Event::AgentInterruptInjected {
                node_id:    stage_id.node_id().to_string(),
                visit:      stage_id.visit(),
                session_id: entry.session_id.clone(),
                actor:      actor.cloned(),
            });
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
        handle: &dyn ActiveControlHandle,
        item: SteeringMessage,
        emitter: &Emitter,
        stage_id: Option<&StageId>,
    ) {
        if let Some(evicted) = handle.enqueue_bounded(item, PER_SESSION_QUEUE_CAP) {
            emitter.emit(&Event::AgentSteerDropped {
                reason:  AgentSteerDroppedReason::QueueFull,
                count:   1,
                actor:   evicted.actor,
                node_id: stage_id.map(|s| s.node_id().to_string()),
                visit:   stage_id.map(StageId::visit),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use fabro_agent::SessionControlHandle;
    use fabro_types::{Principal, RunEvent, RunId, StageId, SystemActorKind};

    use super::{ActiveControlHandle, SteeringHub, SteeringMessage};
    use crate::event::Emitter;

    fn hub_with_event_names() -> (Arc<SteeringHub>, Arc<Mutex<Vec<String>>>) {
        let emitter = Arc::new(Emitter::new(RunId::new()));
        let names = Arc::new(Mutex::new(Vec::new()));
        let names_for_listener = Arc::clone(&names);
        emitter.on_event(move |event| {
            names_for_listener
                .lock()
                .unwrap()
                .push(event.event_name().to_string());
        });
        (Arc::new(SteeringHub::new(emitter)), names)
    }

    fn hub_with_events() -> (Arc<SteeringHub>, Arc<Mutex<Vec<RunEvent>>>) {
        let emitter = Arc::new(Emitter::new(RunId::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_listener = Arc::clone(&events);
        emitter.on_event(move |event| {
            events_for_listener.lock().unwrap().push(event.clone());
        });
        (Arc::new(SteeringHub::new(emitter)), events)
    }

    fn control_handle(handle: &SessionControlHandle) -> Arc<dyn ActiveControlHandle> {
        Arc::new(handle.clone())
    }

    #[derive(Default)]
    struct FakeAcpControlHandle {
        queue:       Mutex<Vec<SteeringMessage>>,
        interrupted: Mutex<usize>,
    }

    impl FakeAcpControlHandle {
        fn queue_len(&self) -> usize {
            self.queue.lock().unwrap().len()
        }

        fn interrupt_count(&self) -> usize {
            *self.interrupted.lock().unwrap()
        }
    }

    impl ActiveControlHandle for FakeAcpControlHandle {
        fn enqueue_bounded(&self, item: SteeringMessage, _cap: usize) -> Option<SteeringMessage> {
            self.queue.lock().unwrap().push(item);
            None
        }

        fn interrupt(&self, _actor: Option<Principal>) {
            *self.interrupted.lock().unwrap() += 1;
        }

        fn interrupt_then_enqueue_bounded(
            &self,
            item: SteeringMessage,
            cap: usize,
        ) -> Option<SteeringMessage> {
            self.interrupt(None);
            self.enqueue_bounded(item, cap)
        }

        fn has_pending_control_work(&self) -> bool {
            !self.queue.lock().unwrap().is_empty()
        }
    }

    #[test]
    fn deliver_with_no_active_buffers_message() {
        let (hub, names) = hub_with_event_names();
        hub.deliver_steer(
            "hi".into(),
            Some(Principal::System {
                system_kind: SystemActorKind::Engine,
            }),
        );
        assert_eq!(hub.pending_len(), 1);
        assert_eq!(names.lock().unwrap().as_slice(), [
            "run.steer",
            "agent.steer.buffered"
        ]);
    }

    #[test]
    fn drain_pending_at_run_end_clears_buffer() {
        let hub = SteeringHub::for_tests();
        hub.deliver_steer("a".into(), None);
        hub.deliver_steer("b".into(), None);
        assert_eq!(hub.pending_len(), 2);
        hub.drain_pending_at_run_end();
        assert_eq!(hub.pending_len(), 0);
    }

    #[test]
    fn pending_buffer_evicts_oldest_at_cap() {
        let hub = SteeringHub::for_tests();
        for i in 0..(super::PER_RUN_PENDING_CAP + 5) {
            hub.deliver_steer(format!("msg{i}"), None);
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
        hub.deliver_steer("queued1".into(), None);
        hub.deliver_steer("queued2".into(), None);
        assert_eq!(hub.pending_len(), 2);

        let stage = StageId::new("agent-node", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage, "session-a", control_handle(&handle)));
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
        assert!(hub.attach_handle(&stage_a, "session-a", control_handle(&handle_a)));
        assert!(hub.attach_handle(&stage_b, "session-b", control_handle(&handle_b)));

        hub.deliver_steer("hello".into(), None);

        assert_eq!(handle_a.queue_len(), 1);
        assert_eq!(handle_b.queue_len(), 1);
        assert_eq!(hub.pending_len(), 0);
    }

    #[test]
    fn deliver_broadcasts_to_api_and_acp_control_handles() {
        let hub = SteeringHub::for_tests();
        let api_stage = StageId::new("api", 1);
        let acp_stage = StageId::new("acp", 1);
        let api_handle = SessionControlHandle::new();
        let acp_handle = Arc::new(FakeAcpControlHandle::default());
        assert!(hub.attach_handle(&api_stage, "session-api", control_handle(&api_handle)));
        assert!(hub.attach_handle(
            &acp_stage,
            "session-acp",
            Arc::clone(&acp_handle) as Arc<dyn ActiveControlHandle>,
        ));

        hub.deliver_steer("hello".into(), None);
        hub.interrupt(None);

        assert_eq!(api_handle.queue_len(), 1);
        assert_eq!(acp_handle.queue_len(), 1);
        assert_eq!(acp_handle.interrupt_count(), 1);
    }

    #[test]
    fn attach_rejects_different_session_for_same_stage() {
        let hub = SteeringHub::for_tests();
        let stage = StageId::new("a", 1);
        let handle1 = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage, "session-a", control_handle(&handle1)));
        hub.deliver_steer("x".into(), None);
        assert_eq!(handle1.queue_len(), 1);

        let handle2 = SessionControlHandle::new();
        assert!(!hub.attach_handle(&stage, "session-b", control_handle(&handle2)));
        assert_eq!(handle2.queue_len(), 0);
    }

    #[test]
    fn stale_detach_does_not_remove_active_session() {
        let hub = SteeringHub::for_tests();
        let stage = StageId::new("a", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage, "session-a", control_handle(&handle)));

        assert!(!hub.detach(&stage, "session-b"));
        hub.deliver_steer("still-active".into(), None);

        assert_eq!(handle.queue_len(), 1);
        assert_eq!(hub.active_count(), 1);
    }

    #[test]
    fn detach_if_no_pending_control_work_respects_session_id_and_queue_state() {
        let hub = SteeringHub::for_tests();
        let stage = StageId::new("a", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage, "session-a", control_handle(&handle)));

        assert!(!hub.detach_if_no_pending_control_work(&stage, "session-b", &handle));
        hub.deliver_steer("queued".into(), None);
        assert!(!hub.detach_if_no_pending_control_work(&stage, "session-a", &handle));
        assert_eq!(hub.active_count(), 1);
    }

    #[test]
    fn detach_if_no_pending_control_work_removes_matching_empty_session() {
        let hub = SteeringHub::for_tests();
        let stage = StageId::new("a", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage, "session-a", control_handle(&handle)));

        assert!(hub.detach_if_no_pending_control_work(&stage, "session-a", &handle));
        assert_eq!(hub.active_count(), 0);
    }

    #[test]
    fn pure_interrupt_marks_active_sessions_waiting_without_queueing_text() {
        let (hub, events) = hub_with_events();
        let stage = StageId::new("a", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage, "session-a", control_handle(&handle)));

        hub.interrupt(None);
        hub.interrupt(None);

        assert!(handle.is_waiting_for_steer());
        assert_eq!(handle.queue_len(), 0);
        assert_eq!(hub.pending_len(), 0);
        let events = events.lock().unwrap();
        let names = events.iter().map(RunEvent::event_name).collect::<Vec<_>>();
        assert_eq!(names, [
            "run.interrupt",
            "agent.interrupt.injected",
            "run.interrupt",
            "agent.interrupt.injected",
        ]);
        assert_eq!(events[1].stage_id, Some(stage.clone()));
        assert_eq!(events[1].session_id.as_deref(), Some("session-a"));
        assert_eq!(events[3].stage_id, Some(stage));
        assert_eq!(events[3].session_id.as_deref(), Some("session-a"));
    }

    #[test]
    fn interrupt_then_steer_cancels_and_queues_text() {
        let (hub, events) = hub_with_events();
        let stage = StageId::new("a", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage, "session-a", control_handle(&handle)));

        hub.interrupt_then_steer("stop", None);

        assert!(!handle.is_waiting_for_steer());
        assert_eq!(handle.queue_len(), 1);
        assert_eq!(hub.pending_len(), 0);
        let events = events.lock().unwrap();
        let names = events.iter().map(RunEvent::event_name).collect::<Vec<_>>();
        assert_eq!(names, [
            "run.interrupt",
            "run.steer",
            "agent.interrupt.injected",
        ]);
        assert_eq!(events[2].stage_id, Some(stage));
        assert_eq!(events[2].session_id.as_deref(), Some("session-a"));
    }

    #[test]
    fn per_session_queue_evicts_oldest_at_cap() {
        let hub = SteeringHub::for_tests();
        let stage = StageId::new("a", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_handle(&stage, "session-a", control_handle(&handle)));

        for i in 0..(super::PER_SESSION_QUEUE_CAP + 5) {
            hub.deliver_steer(format!("m{i}"), None);
        }
        assert_eq!(handle.queue_len(), super::PER_SESSION_QUEUE_CAP);
    }
}
