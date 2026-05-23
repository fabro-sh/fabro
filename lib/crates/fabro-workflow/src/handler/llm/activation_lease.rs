use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fabro_model::{ReasoningEffort, Speed};
use fabro_types::{PermissionLevel, SessionCapability, StageId};

use crate::error::Error;
use crate::event::{Emitter, Event};
use crate::steering_hub::{ActiveControlHandle, SteeringHub};

pub struct ActivationLease {
    stage_id:   StageId,
    session_id: String,
    hub:        Arc<SteeringHub>,
    emitter:    Arc<Emitter>,
    released:   AtomicBool,
}

pub struct ActivationLeaseOptions {
    pub stage_id:         StageId,
    pub session_id:       String,
    pub thread_id:        Option<String>,
    pub provider:         Option<String>,
    pub model:            Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub speed:            Option<Speed>,
    pub permission_level: Option<PermissionLevel>,
    pub capabilities:     Vec<SessionCapability>,
    pub hub:              Arc<SteeringHub>,
    pub emitter:          Arc<Emitter>,
}

impl ActivationLease {
    pub fn activate(
        options: ActivationLeaseOptions,
        handle: &Arc<dyn ActiveControlHandle>,
    ) -> Result<Arc<Self>, Error> {
        let attached = if let Some(pair_handle) = handle.pair_handle() {
            options
                .hub
                .attach_pairable_handle(&options.stage_id, &options.session_id, pair_handle)
        } else {
            options
                .hub
                .attach_handle(&options.stage_id, &options.session_id, Arc::clone(handle))
        };
        if !attached {
            return Err(Error::Precondition(format!(
                "stage {} already has a different active agent session",
                options.stage_id
            )));
        }

        options.emitter.emit(&Event::AgentSessionActivated {
            node_id:          options.stage_id.node_id().to_string(),
            visit:            options.stage_id.visit(),
            session_id:       options.session_id.clone(),
            thread_id:        options.thread_id,
            provider:         options.provider,
            model:            options.model,
            reasoning_effort: options.reasoning_effort,
            speed:            options.speed,
            permission_level: options.permission_level,
            capabilities:     options.capabilities,
        });
        options
            .hub
            .drain_pending_into(&options.stage_id, handle.as_ref());

        Ok(Arc::new(Self {
            stage_id:   options.stage_id,
            session_id: options.session_id,
            hub:        options.hub,
            emitter:    options.emitter,
            released:   AtomicBool::new(false),
        }))
    }

    pub fn release(&self) {
        if !self.mark_released() {
            return;
        }
        self.hub.detach(&self.stage_id, &self.session_id);
    }

    pub fn release_if_no_pending_control_work(&self, handle: &dyn ActiveControlHandle) -> bool {
        if self.released.load(Ordering::Acquire) {
            return true;
        }
        if !self
            .hub
            .detach_if_no_pending_control_work(&self.stage_id, &self.session_id, handle)
        {
            return false;
        }
        self.mark_released();
        true
    }

    pub fn is_pair_active(&self) -> bool {
        !self.released.load(Ordering::Acquire)
            && self
                .hub
                .pair_is_active_for(&self.stage_id, &self.session_id)
    }

    fn mark_released(&self) -> bool {
        if self
            .released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.emitter.emit(&Event::AgentSessionDeactivated {
            node_id:    self.stage_id.node_id().to_string(),
            visit:      self.stage_id.visit(),
            session_id: self.session_id.clone(),
        });
        true
    }
}

impl Drop for ActivationLease {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use fabro_agent::SessionControlHandle;
    use fabro_types::RunId;

    use super::*;

    fn collect_event_names(emitter: &Arc<Emitter>) -> Arc<Mutex<Vec<String>>> {
        let names = Arc::new(Mutex::new(Vec::new()));
        let names_for_listener = Arc::clone(&names);
        emitter.on_event(move |event| {
            names_for_listener
                .lock()
                .unwrap()
                .push(event.event_name().to_string());
        });
        names
    }

    fn options(
        stage_id: StageId,
        session_id: &str,
        hub: Arc<SteeringHub>,
        emitter: Arc<Emitter>,
    ) -> ActivationLeaseOptions {
        ActivationLeaseOptions {
            stage_id,
            session_id: session_id.to_string(),
            thread_id: None,
            provider: Some("openai".to_string()),
            model: Some("gpt-5.4".to_string()),
            reasoning_effort: None,
            speed: None,
            permission_level: None,
            capabilities: vec![SessionCapability::Steer],
            hub,
            emitter,
        }
    }

    fn control_handle(handle: &SessionControlHandle) -> Arc<dyn ActiveControlHandle> {
        Arc::new(handle.clone())
    }

    #[test]
    fn activate_emits_activated_before_draining_pending() {
        let emitter = Arc::new(Emitter::new(RunId::new()));
        let names = collect_event_names(&emitter);
        let hub = Arc::new(SteeringHub::new(Arc::clone(&emitter)));
        let stage_id = StageId::new("agent", 1);
        let handle = SessionControlHandle::new();

        hub.deliver_steer("queued".to_string(), None);
        let _lease = ActivationLease::activate(
            options(
                stage_id.clone(),
                "session-a",
                Arc::clone(&hub),
                Arc::clone(&emitter),
            ),
            &control_handle(&handle),
        )
        .unwrap();

        assert_eq!(handle.queue_len(), 1);
        assert_eq!(names.lock().unwrap().as_slice(), [
            "run.steer",
            "agent.steer.buffered",
            "agent.session.activated"
        ]);
    }

    #[test]
    fn activate_rejects_mismatched_existing_session() {
        let emitter = Arc::new(Emitter::new(RunId::new()));
        let names = collect_event_names(&emitter);
        let hub = Arc::new(SteeringHub::new(Arc::clone(&emitter)));
        let stage_id = StageId::new("agent", 1);
        let handle_a = SessionControlHandle::new();
        let handle_b = SessionControlHandle::new();

        let _lease = ActivationLease::activate(
            options(
                stage_id.clone(),
                "session-a",
                Arc::clone(&hub),
                Arc::clone(&emitter),
            ),
            &control_handle(&handle_a),
        )
        .unwrap();
        let result = ActivationLease::activate(
            options(
                stage_id,
                "session-b",
                Arc::clone(&hub),
                Arc::clone(&emitter),
            ),
            &control_handle(&handle_b),
        );

        assert!(result.is_err());
        assert_eq!(handle_b.queue_len(), 0);
        assert_eq!(
            names
                .lock()
                .unwrap()
                .iter()
                .filter(|name| name.as_str() == "agent.session.activated")
                .count(),
            1
        );
    }

    #[test]
    fn release_is_idempotent() {
        let emitter = Arc::new(Emitter::new(RunId::new()));
        let names = collect_event_names(&emitter);
        let hub = Arc::new(SteeringHub::new(Arc::clone(&emitter)));
        let stage_id = StageId::new("agent", 1);
        let handle = SessionControlHandle::new();

        let lease = ActivationLease::activate(
            options(
                stage_id,
                "session-a",
                Arc::clone(&hub),
                Arc::clone(&emitter),
            ),
            &control_handle(&handle),
        )
        .unwrap();
        lease.release();
        lease.release();

        assert_eq!(
            names
                .lock()
                .unwrap()
                .iter()
                .filter(|name| name.as_str() == "agent.session.deactivated")
                .count(),
            1
        );
    }
}
