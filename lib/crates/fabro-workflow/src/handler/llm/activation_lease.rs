use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fabro_agent::SessionControlHandle;
use fabro_types::{SessionCapability, StageId};

use crate::error::Error;
use crate::event::{Emitter, Event};
use crate::steering_hub::SteeringHub;

pub struct ActivationLease {
    stage_id:   StageId,
    session_id: String,
    hub:        Arc<SteeringHub>,
    emitter:    Arc<Emitter>,
    released:   AtomicBool,
}

pub struct ActivationLeaseOptions {
    pub stage_id:     StageId,
    pub session_id:   String,
    pub thread_id:    Option<String>,
    pub provider:     Option<String>,
    pub model:        Option<String>,
    pub capabilities: Vec<SessionCapability>,
    pub hub:          Arc<SteeringHub>,
    pub emitter:      Arc<Emitter>,
}

impl ActivationLease {
    pub fn activate(
        options: ActivationLeaseOptions,
        handle: &SessionControlHandle,
    ) -> Result<Arc<Self>, Error> {
        if !options
            .hub
            .attach_handle(&options.stage_id, &options.session_id, handle)
        {
            return Err(Error::Precondition(format!(
                "stage {} already has a different active agent session",
                options.stage_id
            )));
        }

        options.emitter.emit(&Event::AgentSessionActivated {
            node_id:      options.stage_id.node_id().to_string(),
            visit:        options.stage_id.visit(),
            session_id:   options.session_id.clone(),
            thread_id:    options.thread_id,
            provider:     options.provider,
            model:        options.model,
            capabilities: options.capabilities,
        });
        options.hub.drain_pending_into(&options.stage_id, handle);

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

    pub fn release_if_no_pending_control_work(&self, handle: &SessionControlHandle) -> bool {
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
            capabilities: vec![SessionCapability::Steer],
            hub,
            emitter,
        }
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
            &handle,
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
            &handle_a,
        )
        .unwrap();
        let result = ActivationLease::activate(
            options(
                stage_id,
                "session-b",
                Arc::clone(&hub),
                Arc::clone(&emitter),
            ),
            &handle_b,
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
            &handle,
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
