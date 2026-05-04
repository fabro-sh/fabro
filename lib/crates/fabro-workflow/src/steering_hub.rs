use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use fabro_agent::SessionControlHandle;
use fabro_types::{Principal, StageId, SteerKind};

use crate::event::{Emitter, Event};

/// Per-session steering queue capacity.
pub const PER_SESSION_QUEUE_CAP: usize = 32;
/// Per-run pending buffer capacity.
pub const PER_RUN_PENDING_CAP: usize = 32;

struct PendingSteer {
    text:  String,
    actor: Option<Principal>,
}

/// Coordinates steering message delivery across active agent sessions.
///
/// All locks are `std::sync` — methods are synchronous and never `.await`
/// while holding any lock. Lock order: active first, then pending.
pub struct SteeringHub {
    active:  RwLock<HashMap<StageId, SessionControlHandle>>,
    pending: Mutex<VecDeque<PendingSteer>>,
    emitter: Arc<Emitter>,
}

impl SteeringHub {
    pub fn new(emitter: Arc<Emitter>) -> Self {
        Self {
            active: RwLock::new(HashMap::new()),
            pending: Mutex::new(VecDeque::new()),
            emitter,
        }
    }

    /// Deliver a steer message. Broadcasts to all active sessions or buffers
    /// if none are registered.
    pub fn deliver(&self, text: &str, kind: SteerKind, actor: Option<Principal>) {
        let active = self.active.read().expect("steering active lock poisoned");
        if active.is_empty() {
            drop(active);
            let mut pending = self.pending.lock().expect("steering pending lock poisoned");
            if pending.len() >= PER_RUN_PENDING_CAP {
                let dropped = pending.pop_front();
                if let Some(dropped) = dropped {
                    self.emitter.emit(&Event::SteerDropped {
                        count:  1,
                        reason: "queue_full".to_string(),
                        actor:  dropped.actor,
                    });
                }
            }
            pending.push_back(PendingSteer {
                text:  text.to_string(),
                actor: actor.clone(),
            });
            self.emitter.emit(&Event::SteerBuffered { kind, actor });
        } else {
            for (_, handle) in active.iter() {
                Self::enqueue_into_session(handle, text, kind, actor.as_ref());
            }
        }
    }

    /// Register a session handle for a stage. If the stage is newly inserted
    /// (not already active), drains pending steers into it.
    pub fn register(&self, stage_id: &StageId, handle: &SessionControlHandle) {
        let mut active = self.active.write().expect("steering active lock poisoned");
        let is_new = !active.contains_key(stage_id);
        active.insert(stage_id.clone(), handle.clone());

        if is_new {
            // Drain pending into the newly registered session
            let mut pending = self.pending.lock().expect("steering pending lock poisoned");
            for steer in pending.drain(..) {
                // Buffered steers always delivered as append
                Self::enqueue_into_session(
                    handle,
                    &steer.text,
                    SteerKind::Append,
                    steer.actor.as_ref(),
                );
            }
            drop(pending);

            self.emitter.emit(&Event::SteeringAttached {
                stage: stage_id.to_string(),
            });
        }
    }

    /// Unregister a session handle. Idempotent — emits `detached` only when
    /// the entry was actually present.
    pub fn unregister(&self, stage_id: &StageId) {
        let mut active = self.active.write().expect("steering active lock poisoned");
        if active.remove(stage_id).is_some() {
            self.emitter.emit(&Event::SteeringDetached {
                stage: stage_id.to_string(),
            });
        }
    }

    /// Emit `agent.steer.dropped` for any remaining pending steers at run end.
    pub fn drain_pending_at_run_end(&self) {
        let mut pending = self.pending.lock().expect("steering pending lock poisoned");
        let count = pending.len();
        if count > 0 {
            pending.clear();
            self.emitter.emit(&Event::SteerDropped {
                count,
                reason: "run_ended".to_string(),
                actor: None,
            });
        }
    }

    fn enqueue_into_session(
        handle: &SessionControlHandle,
        text: &str,
        kind: SteerKind,
        actor: Option<&Principal>,
    ) {
        match kind {
            SteerKind::Append => handle.steer(text.to_string(), actor.cloned()),
            SteerKind::Interrupt => handle.interrupt_with(text.to_string(), actor.cloned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use fabro_agent::SessionControlHandle;
    use fabro_types::{StageId, SteerKind};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::event::Emitter;

    fn test_emitter() -> Arc<Emitter> {
        Arc::new(Emitter::new(fabro_types::fixtures::RUN_1))
    }

    #[expect(
        clippy::type_complexity,
        reason = "Test helper returns internal queue handle for direct inspection."
    )]
    fn test_handle() -> (
        SessionControlHandle,
        Arc<Mutex<VecDeque<(String, SteerKind, Option<Principal>)>>>,
    ) {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let round_token = Arc::new(std::sync::RwLock::new(CancellationToken::new()));
        let handle = SessionControlHandle::new(queue.clone(), round_token);
        (handle, queue)
    }

    #[test]
    fn deliver_broadcasts_to_active_sessions() {
        let hub = SteeringHub::new(test_emitter());
        let (handle, queue) = test_handle();
        let stage = StageId::new("node1", 1);
        hub.register(&stage, &handle);

        hub.deliver("hello", SteerKind::Append, None);

        let q = queue.lock().unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].0, "hello");
    }

    #[test]
    fn deliver_buffers_when_no_active() {
        let hub = SteeringHub::new(test_emitter());
        hub.deliver("hello", SteerKind::Append, None);

        // Register drains pending
        let (handle, queue) = test_handle();
        let stage = StageId::new("node1", 1);
        hub.register(&stage, &handle);

        let q = queue.lock().unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].0, "hello");
    }

    #[test]
    fn register_drains_pending_only_on_new_insert() {
        let hub = SteeringHub::new(test_emitter());
        hub.deliver("buffered", SteerKind::Append, None);

        let (handle1, queue1) = test_handle();
        let stage = StageId::new("node1", 1);
        hub.register(&stage, &handle1);
        assert_eq!(queue1.lock().unwrap().len(), 1);

        // Second register with same stage_id should NOT drain again
        let (handle2, queue2) = test_handle();
        hub.register(&stage, &handle2);
        assert_eq!(queue2.lock().unwrap().len(), 0);
    }

    #[test]
    fn unregister_is_idempotent() {
        let hub = SteeringHub::new(test_emitter());
        let (handle, _queue) = test_handle();
        let stage = StageId::new("node1", 1);
        hub.register(&stage, &handle);

        hub.unregister(&stage);
        hub.unregister(&stage); // no panic, no double emit
    }

    #[test]
    fn drain_pending_at_run_end_clears_buffer() {
        let hub = SteeringHub::new(test_emitter());
        hub.deliver("a", SteerKind::Append, None);
        hub.deliver("b", SteerKind::Interrupt, None);

        hub.drain_pending_at_run_end();

        // Registering after drain should get nothing
        let (handle, queue) = test_handle();
        hub.register(&StageId::new("node1", 1), &handle);
        assert_eq!(queue.lock().unwrap().len(), 0);
    }

    #[test]
    fn pending_overflow_drops_oldest() {
        let hub = SteeringHub::new(test_emitter());
        for i in 0..PER_RUN_PENDING_CAP + 5 {
            hub.deliver(&format!("msg-{i}"), SteerKind::Append, None);
        }

        let (handle, queue) = test_handle();
        hub.register(&StageId::new("node1", 1), &handle);
        let q = queue.lock().unwrap();
        assert_eq!(q.len(), PER_RUN_PENDING_CAP);
        // Oldest messages should have been dropped
        assert_eq!(q[0].0, "msg-5");
    }

    #[test]
    fn interrupt_cancels_round_token() {
        let hub = SteeringHub::new(test_emitter());
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let round_token = Arc::new(std::sync::RwLock::new(CancellationToken::new()));
        let rt_clone = round_token
            .read()
            .expect("round token lock poisoned")
            .clone();
        let handle = SessionControlHandle::new(queue.clone(), round_token);
        let stage = StageId::new("node1", 1);
        hub.register(&stage, &handle);

        hub.deliver("stop", SteerKind::Interrupt, None);

        assert!(rt_clone.is_cancelled());
        let q = queue.lock().unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].1, SteerKind::Interrupt);
    }
}
