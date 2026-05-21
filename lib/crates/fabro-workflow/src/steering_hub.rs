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

use chrono::Utc;
use fabro_agent::{SessionControlHandle, SteeringItem};
use fabro_types::run_event::AgentSteerDroppedReason;
use fabro_types::{
    PairId, PairMessageId, PairMessageRecord, PairRecord, PairStatus, PairSystemMessageKind,
    PairTarget, Principal, RunId, RunPairEndedReason, StageId,
};

use crate::event::{Emitter, Event};

/// Cap on the steering queue length kept per active session. Overflow
/// evicts the oldest entry (FIFO) and emits `agent.steer.dropped`.
pub const PER_SESSION_QUEUE_CAP: usize = 32;

/// Cap on the run-wide pending buffer used when no session is registered.
/// Overflow evicts the oldest entry (FIFO) and emits `agent.steer.dropped`.
pub const PER_RUN_PENDING_CAP: usize = 32;

pub trait ActiveControlHandle: Send + Sync {
    fn enqueue_bounded(&self, item: SteeringItem, cap: usize) -> Option<SteeringItem>;
    fn interrupt(&self, actor: Option<Principal>);
    fn interrupt_then_enqueue_bounded(
        &self,
        item: SteeringItem,
        cap: usize,
    ) -> Option<SteeringItem>;
    fn park_for_steer(&self) {}
    fn pair_handle(&self) -> Option<SessionControlHandle> {
        None
    }
    fn has_pending_control_work(&self) -> bool;
}

impl ActiveControlHandle for SessionControlHandle {
    fn enqueue_bounded(&self, item: SteeringItem, cap: usize) -> Option<SteeringItem> {
        Self::enqueue_bounded(self, item, cap)
    }

    fn interrupt(&self, actor: Option<Principal>) {
        Self::interrupt(self, actor);
    }

    fn interrupt_then_enqueue_bounded(
        &self,
        item: SteeringItem,
        cap: usize,
    ) -> Option<SteeringItem> {
        Self::interrupt_then_enqueue_bounded(self, item, cap)
    }

    fn park_for_steer(&self) {
        Self::park_for_steer(self);
    }

    fn pair_handle(&self) -> Option<SessionControlHandle> {
        Some(self.clone())
    }

    fn has_pending_control_work(&self) -> bool {
        Self::has_pending_control_work(self)
    }
}

#[derive(Clone)]
struct ActiveEntry {
    handle:      Arc<dyn ActiveControlHandle>,
    pair_handle: Option<SessionControlHandle>,
    session_id:  String,
}

#[derive(Debug, Clone)]
struct ActivePair {
    record:     PairRecord,
    /// Snapshot of the agent session id active at `start_pair` time. Used to
    /// detect session replacement on subsequent pair commands and on
    /// `AgentSessionDeactivated` cleanup; intentionally not exposed in the
    /// public `PairRecord`.
    session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairControlError {
    AlreadyPaired,
    PairNotCurrent,
    PairNotActive,
    TargetNotActive,
    MessageNotAccepted,
}

#[allow(
    clippy::module_name_repetitions,
    reason = "external callers refer to it as SteeringHub"
)]
pub struct SteeringHub {
    active:      RwLock<HashMap<StageId, ActiveEntry>>,
    active_pair: Mutex<Option<ActivePair>>,
    pending:     Mutex<VecDeque<SteeringItem>>,
    emitter:     Arc<Emitter>,
}

impl SteeringHub {
    #[must_use]
    pub fn new(emitter: Arc<Emitter>) -> Self {
        Self {
            active: RwLock::new(HashMap::new()),
            active_pair: Mutex::new(None),
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
        self.attach_entry(stage_id, session_id, handle, None)
    }

    /// Attach a native API session as steerable and pairable for this stage.
    pub fn attach_pairable_handle(
        &self,
        stage_id: &StageId,
        session_id: &str,
        handle: SessionControlHandle,
    ) -> bool {
        self.attach_entry(
            stage_id,
            session_id,
            Arc::new(handle.clone()) as Arc<dyn ActiveControlHandle>,
            Some(handle),
        )
    }

    fn attach_entry(
        &self,
        stage_id: &StageId,
        session_id: &str,
        handle: Arc<dyn ActiveControlHandle>,
        pair_handle: Option<SessionControlHandle>,
    ) -> bool {
        let mut active = self.active.write().expect("active lock poisoned");
        match active.get_mut(stage_id) {
            Some(entry) if entry.session_id != session_id => false,
            Some(entry) => {
                entry.handle = handle;
                if pair_handle.is_some() {
                    entry.pair_handle = pair_handle;
                }
                true
            }
            None => {
                active.insert(stage_id.clone(), ActiveEntry {
                    handle,
                    pair_handle,
                    session_id: session_id.to_string(),
                });
                true
            }
        }
    }

    /// Drain pending run-wide steers into `handle`.
    pub fn drain_pending_into(&self, stage_id: &StageId, handle: &dyn ActiveControlHandle) {
        let pending: Vec<SteeringItem> = {
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
        drop(active);
        self.end_active_pair_for_target(stage_id, session_id, RunPairEndedReason::SessionEnded);
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
        drop(active);
        self.end_active_pair_for_target(stage_id, session_id, RunPairEndedReason::SessionEnded);
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
                    pending.pop_front().and_then(|d| d.actor().cloned())
                } else {
                    None
                };
                pending.push_back(SteeringItem::Steering {
                    text,
                    actor: actor.clone(),
                });
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
                SteeringItem::Steering {
                    text:  text.clone(),
                    actor: actor.clone(),
                },
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
                SteeringItem::Steering {
                    text:  text.to_string(),
                    actor: actor.cloned(),
                },
                PER_SESSION_QUEUE_CAP,
            ) {
                self.emitter.emit(&Event::AgentSteerDropped {
                    reason:  AgentSteerDroppedReason::QueueFull,
                    count:   1,
                    actor:   evicted.actor().cloned(),
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
        self.end_active_pair(RunPairEndedReason::RunEnded);
    }

    pub fn start_pair(
        &self,
        run_id: RunId,
        pair_id: PairId,
        target: PairTarget,
        actor: Option<Principal>,
    ) -> Result<PairRecord, PairControlError> {
        let active = self.active.read().expect("active lock poisoned");
        let Some(entry) = active.get(&target.stage_id) else {
            return Err(PairControlError::TargetNotActive);
        };
        let Some(pair_handle) = entry.pair_handle.as_ref() else {
            return Err(PairControlError::TargetNotActive);
        };
        let session_id = entry.session_id.clone();
        let interrupt_handle = Arc::clone(&entry.handle);
        let pair_handle = pair_handle.clone();
        drop(active);

        let mut active_pair = self.active_pair.lock().expect("active pair lock poisoned");
        if active_pair.is_some() {
            return Err(PairControlError::AlreadyPaired);
        }

        let text = human_joined_text();
        if !pair_handle.try_enqueue_bounded(
            SteeringItem::System {
                text: text.to_string(),
            },
            PER_SESSION_QUEUE_CAP,
        ) {
            return Err(PairControlError::MessageNotAccepted);
        }

        let record = PairRecord {
            pair_id,
            run_id,
            status: PairStatus::Active,
            started_at: Utc::now(),
            ended_at: None,
            failure_reason: None,
            target,
        };
        self.emitter.emit(&Event::RunPairStarted {
            pair_id,
            target: record.target.clone(),
            actor: actor.clone(),
        });

        interrupt_handle.interrupt(actor);
        self.emitter.emit(&Event::AgentPairSystemMessage {
            node_id: record.target.stage_id.node_id().to_string(),
            visit: record.target.stage_id.visit(),
            session_id: session_id.clone(),
            pair_id,
            kind: PairSystemMessageKind::HumanJoined,
            text: text.to_string(),
        });
        *active_pair = Some(ActivePair {
            record: record.clone(),
            session_id,
        });
        Ok(record)
    }

    pub fn send_pair_message(
        &self,
        pair_id: PairId,
        message_id: PairMessageId,
        text: String,
        client_message_id: Option<String>,
        actor: Option<Principal>,
    ) -> Result<PairMessageRecord, PairControlError> {
        let active_pair = self.active_pair.lock().expect("active pair lock poisoned");
        let Some(pair) = active_pair.as_ref() else {
            return Err(PairControlError::PairNotActive);
        };
        if pair.record.pair_id != pair_id {
            return Err(PairControlError::PairNotCurrent);
        }
        if pair.record.status != PairStatus::Active {
            return Err(PairControlError::PairNotActive);
        }

        let target = &pair.record.target;
        let session_id = pair.session_id.clone();
        let active = self.active.read().expect("active lock poisoned");
        let Some(entry) = active.get(&target.stage_id) else {
            return Err(PairControlError::TargetNotActive);
        };
        if entry.session_id != session_id {
            return Err(PairControlError::TargetNotActive);
        }
        let Some(pair_handle) = entry.pair_handle.as_ref() else {
            return Err(PairControlError::TargetNotActive);
        };

        if !pair_handle.try_enqueue_bounded(
            SteeringItem::User { text: text.clone() },
            PER_SESSION_QUEUE_CAP,
        ) {
            return Err(PairControlError::MessageNotAccepted);
        }
        self.emitter.emit(&Event::AgentPairUserMessage {
            node_id: target.stage_id.node_id().to_string(),
            visit: target.stage_id.visit(),
            session_id,
            pair_id,
            message_id,
            client_message_id: client_message_id.clone(),
            text: text.clone(),
            actor,
        });
        Ok(PairMessageRecord {
            message_id,
            client_message_id,
            pair_id,
            run_id: pair.record.run_id,
            stage_id: target.stage_id.clone(),
            text,
            accepted_at: Utc::now(),
        })
    }

    pub fn end_pair(
        &self,
        pair_id: PairId,
        actor: Option<Principal>,
    ) -> Result<PairRecord, PairControlError> {
        let mut active_pair = self.active_pair.lock().expect("active pair lock poisoned");
        let Some(pair) = active_pair.as_mut() else {
            return Err(PairControlError::PairNotActive);
        };
        if pair.record.pair_id != pair_id {
            return Err(PairControlError::PairNotCurrent);
        }
        if pair.record.status != PairStatus::Active {
            return Err(PairControlError::PairNotActive);
        }

        let target = pair.record.target.clone();
        let session_id = pair.session_id.clone();
        let text = human_left_text();
        if let Some(entry) = self
            .active
            .read()
            .expect("active lock poisoned")
            .get(&target.stage_id)
            .filter(|entry| entry.session_id == session_id)
        {
            let Some(pair_handle) = entry.pair_handle.as_ref() else {
                return Err(PairControlError::TargetNotActive);
            };
            if !pair_handle.try_enqueue_bounded(
                SteeringItem::System {
                    text: text.to_string(),
                },
                PER_SESSION_QUEUE_CAP,
            ) {
                return Err(PairControlError::MessageNotAccepted);
            }
            self.emitter.emit(&Event::AgentPairSystemMessage {
                node_id: target.stage_id.node_id().to_string(),
                visit: target.stage_id.visit(),
                session_id: session_id.clone(),
                pair_id,
                kind: PairSystemMessageKind::HumanLeft,
                text: text.to_string(),
            });
        }

        pair.record.status = PairStatus::Ended;
        pair.record.ended_at = Some(Utc::now());
        self.emitter.emit(&Event::RunPairEnded {
            pair_id,
            reason: fabro_types::RunPairEndedReason::UserRequested,
            actor,
        });
        let record = pair.record.clone();
        *active_pair = None;
        Ok(record)
    }

    #[must_use]
    pub fn pair_is_active_for(&self, stage_id: &StageId, session_id: &str) -> bool {
        self.active_pair
            .lock()
            .expect("active pair lock poisoned")
            .as_ref()
            .is_some_and(|pair| {
                pair.record.status == PairStatus::Active
                    && pair.record.target.stage_id == *stage_id
                    && pair.session_id == session_id
            })
    }

    fn end_active_pair_for_target(
        &self,
        stage_id: &StageId,
        session_id: &str,
        reason: RunPairEndedReason,
    ) -> bool {
        let pair_id = {
            let mut active_pair = self.active_pair.lock().expect("active pair lock poisoned");
            let Some(pair) = active_pair.as_ref() else {
                return false;
            };
            if pair.record.status != PairStatus::Active
                || pair.record.target.stage_id != *stage_id
                || pair.session_id != session_id
            {
                return false;
            }
            let pair_id = pair.record.pair_id;
            *active_pair = None;
            pair_id
        };
        self.emitter.emit(&Event::RunPairEnded {
            pair_id,
            reason,
            actor: None,
        });
        true
    }

    fn end_active_pair(&self, reason: RunPairEndedReason) -> bool {
        let pair_id = {
            let mut active_pair = self.active_pair.lock().expect("active pair lock poisoned");
            let Some(mut pair) = active_pair.take() else {
                return false;
            };
            if pair.record.status != PairStatus::Active {
                *active_pair = Some(pair);
                return false;
            }
            pair.record.status = PairStatus::Ended;
            pair.record.ended_at = Some(Utc::now());
            pair.record.pair_id
        };
        self.emitter.emit(&Event::RunPairEnded {
            pair_id,
            reason,
            actor: None,
        });
        true
    }

    /// Push an item into a session's queue, evicting the oldest entry and
    /// emitting `agent.steer.dropped { queue_full }` if the cap is hit.
    /// The push + eviction are atomic under the per-session queue lock.
    fn enqueue_into_session_queue(
        handle: &dyn ActiveControlHandle,
        item: SteeringItem,
        emitter: &Emitter,
        stage_id: Option<&StageId>,
    ) {
        if let Some(evicted) = handle.enqueue_bounded(item, PER_SESSION_QUEUE_CAP) {
            emitter.emit(&Event::AgentSteerDropped {
                reason:  AgentSteerDroppedReason::QueueFull,
                count:   1,
                actor:   evicted.actor().cloned(),
                node_id: stage_id.map(|s| s.node_id().to_string()),
                visit:   stage_id.map(StageId::visit),
            });
        }
    }
}

pub fn human_joined_text() -> &'static str {
    "A human has joined this workflow run for live pairing. Wait for their next message before continuing."
}

pub fn human_left_text() -> &'static str {
    "The human has ended live pairing. Continue autonomously with the workflow."
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use fabro_agent::{SessionControlHandle, SteeringItem};
    use fabro_types::{
        PairId, PairMessageId, PairTarget, Principal, RunEvent, RunId, StageId, SystemActorKind,
    };

    use super::{ActiveControlHandle, PairControlError, SteeringHub};
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

    fn pair_target(stage_id: &StageId, _session_id: &str) -> PairTarget {
        PairTarget {
            stage_id:   stage_id.clone(),
            node_label: stage_id.node_id().to_string(),
        }
    }

    fn control_handle(handle: &SessionControlHandle) -> Arc<dyn ActiveControlHandle> {
        Arc::new(handle.clone())
    }

    #[derive(Default)]
    struct FakeAcpControlHandle {
        queue:       Mutex<Vec<SteeringItem>>,
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
        fn enqueue_bounded(&self, item: SteeringItem, _cap: usize) -> Option<SteeringItem> {
            self.queue.lock().unwrap().push(item);
            None
        }

        fn interrupt(&self, _actor: Option<Principal>) {
            *self.interrupted.lock().unwrap() += 1;
        }

        fn interrupt_then_enqueue_bounded(
            &self,
            item: SteeringItem,
            cap: usize,
        ) -> Option<SteeringItem> {
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
    fn pair_start_message_and_end_emit_typed_events_for_selected_target() {
        let (hub, events) = hub_with_events();
        let stage_id = StageId::new("code", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_pairable_handle(&stage_id, "ses_01", handle.clone()));
        let pair_id = PairId::new();

        let started = hub
            .start_pair(
                RunId::new(),
                pair_id,
                pair_target(&stage_id, "ses_01"),
                None,
            )
            .unwrap();
        assert_eq!(started.status, fabro_types::PairStatus::Active);
        assert_eq!(handle.queue_len(), 1);
        assert!(hub.pair_is_active_for(&stage_id, "ses_01"));

        let message = hub
            .send_pair_message(
                pair_id,
                PairMessageId::new(),
                "please inspect this".to_string(),
                Some("client-1".to_string()),
                None,
            )
            .unwrap();
        assert_eq!(message.text, "please inspect this");
        assert_eq!(handle.queue_len(), 2);

        let ended = hub.end_pair(pair_id, None).unwrap();
        assert_eq!(ended.status, fabro_types::PairStatus::Ended);
        assert!(!hub.pair_is_active_for(&stage_id, "ses_01"));
        assert_eq!(handle.queue_len(), 3);

        let names = events
            .lock()
            .unwrap()
            .iter()
            .map(|event| event.event_name().to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, [
            "run.pair.started",
            "agent.pair.system_message",
            "agent.pair.user_message",
            "agent.pair.system_message",
            "run.pair.ended"
        ]);
    }

    #[test]
    fn pair_start_rejects_missing_target() {
        let hub = SteeringHub::for_tests();
        let stage_id = StageId::new("code", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_pairable_handle(&stage_id, "ses_01", handle.clone()));

        let missing_stage = StageId::new("other", 1);
        let result = hub.start_pair(
            RunId::new(),
            PairId::new(),
            pair_target(&missing_stage, "ses_01"),
            None,
        );
        assert_eq!(result.unwrap_err(), PairControlError::TargetNotActive);
    }

    #[test]
    fn detach_ends_active_pair_for_session() {
        let (hub, events) = hub_with_events();
        let stage_id = StageId::new("code", 1);
        let handle = SessionControlHandle::new();
        assert!(hub.attach_pairable_handle(&stage_id, "ses_01", handle.clone()));
        let pair_id = PairId::new();
        hub.start_pair(
            RunId::new(),
            pair_id,
            pair_target(&stage_id, "ses_01"),
            None,
        )
        .unwrap();

        assert!(hub.detach(&stage_id, "ses_01"));

        assert!(!hub.pair_is_active_for(&stage_id, "ses_01"));
        let names = events
            .lock()
            .unwrap()
            .iter()
            .map(|event| event.event_name().to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, [
            "run.pair.started",
            "agent.pair.system_message",
            "run.pair.ended"
        ]);
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
