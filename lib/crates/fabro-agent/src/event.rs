use crate::types::{AgentEvent, SessionEvent};
use std::time::SystemTime;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct EventEmitter {
    sender: broadcast::Sender<SessionEvent>,
}

impl EventEmitter {
    #[must_use]
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(1024);
        Self { sender }
    }

    pub fn emit(&self, session_id: String, event: AgentEvent) {
        event.trace(&session_id);
        let wrapped = SessionEvent {
            event,
            timestamp: SystemTime::now(),
            session_id,
        };
        // Ignore send error (no receivers)
        let _ = self.sender.send(wrapped);
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.sender.subscribe()
    }
}

impl Default for EventEmitter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AgentError;

    #[tokio::test]
    async fn emit_and_receive_event() {
        let emitter = EventEmitter::new();
        let mut receiver = emitter.subscribe();

        emitter.emit("sess-1".into(), AgentEvent::SessionStarted);

        let event = receiver.recv().await.unwrap();
        assert!(matches!(event.event, AgentEvent::SessionStarted));
        assert_eq!(event.session_id, "sess-1");
    }

    #[tokio::test]
    async fn emit_with_data() {
        let emitter = EventEmitter::new();
        let mut receiver = emitter.subscribe();

        emitter.emit(
            "sess-2".into(),
            AgentEvent::Error {
                error: AgentError::ToolExecution("something went wrong".into()),
            },
        );

        let event = receiver.recv().await.unwrap();
        assert!(
            matches!(&event.event, AgentEvent::Error { error } if error.to_string().contains("something went wrong"))
        );
    }

    #[tokio::test]
    async fn multiple_subscribers() {
        let emitter = EventEmitter::new();
        let mut rx1 = emitter.subscribe();
        let mut rx2 = emitter.subscribe();

        emitter.emit("sess-3".into(), AgentEvent::SessionEnded);

        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        assert!(matches!(e1.event, AgentEvent::SessionEnded));
        assert!(matches!(e2.event, AgentEvent::SessionEnded));
        assert_eq!(e1.session_id, "sess-3");
        assert_eq!(e2.session_id, "sess-3");
    }

    #[test]
    fn emit_without_subscribers_does_not_panic() {
        let emitter = EventEmitter::new();
        emitter.emit(
            "sess-4".into(),
            AgentEvent::Error {
                error: AgentError::ToolExecution("test".into()),
            },
        );
    }

    #[test]
    fn default_creates_emitter() {
        let emitter = EventEmitter::default();
        let _rx = emitter.subscribe();
    }
}
