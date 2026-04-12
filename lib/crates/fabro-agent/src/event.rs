use std::time::SystemTime;

use tokio::sync::broadcast;

use crate::types::{AgentEvent, SessionEvent};

#[derive(Clone)]
pub struct Emitter {
    sender: broadcast::Sender<SessionEvent>,
}

impl Emitter {
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
            parent_session_id: None,
        };
        // Ignore send error (no receivers)
        let _ = self.sender.send(wrapped);
    }

    pub fn forward(&self, event: SessionEvent) {
        let _ = self.sender.send(event);
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.sender.subscribe()
    }
}

impl Default for Emitter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    #[tokio::test]
    async fn emit_and_receive_event() {
        let emitter = Emitter::new();
        let mut receiver = emitter.subscribe();

        emitter.emit("sess-1".into(), AgentEvent::SessionStarted {
            provider: Some("anthropic".into()),
            model:    Some("claude-opus".into()),
        });

        let event = receiver.recv().await.unwrap();
        assert!(matches!(event.event, AgentEvent::SessionStarted {
            provider: Some(_),
            model:    Some(_),
        }));
        assert_eq!(event.session_id, "sess-1");
        assert_eq!(event.parent_session_id, None);
    }

    #[tokio::test]
    async fn emit_with_data() {
        let emitter = Emitter::new();
        let mut receiver = emitter.subscribe();

        emitter.emit("sess-2".into(), AgentEvent::Error {
            error: Error::ToolExecution("something went wrong".into()),
        });

        let event = receiver.recv().await.unwrap();
        assert!(
            matches!(&event.event, AgentEvent::Error { error } if error.to_string().contains("something went wrong"))
        );
        assert_eq!(event.parent_session_id, None);
    }

    #[tokio::test]
    async fn multiple_subscribers() {
        let emitter = Emitter::new();
        let mut rx1 = emitter.subscribe();
        let mut rx2 = emitter.subscribe();

        emitter.emit("sess-3".into(), AgentEvent::SessionEnded);

        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        assert!(matches!(e1.event, AgentEvent::SessionEnded));
        assert!(matches!(e2.event, AgentEvent::SessionEnded));
        assert_eq!(e1.session_id, "sess-3");
        assert_eq!(e2.session_id, "sess-3");
        assert_eq!(e1.parent_session_id, None);
        assert_eq!(e2.parent_session_id, None);
    }

    #[test]
    fn emit_without_subscribers_does_not_panic() {
        let emitter = Emitter::new();
        emitter.emit("sess-4".into(), AgentEvent::Error {
            error: Error::ToolExecution("test".into()),
        });
    }

    #[test]
    fn default_creates_emitter() {
        let emitter = Emitter::default();
        let _rx = emitter.subscribe();
    }

    #[tokio::test]
    async fn forward_preserves_session_ids() {
        let emitter = Emitter::new();
        let mut receiver = emitter.subscribe();

        emitter.forward(SessionEvent {
            event:             AgentEvent::SessionStarted {
                provider: Some("anthropic".into()),
                model:    Some("claude-opus".into()),
            },
            timestamp:         SystemTime::now(),
            session_id:        "child".into(),
            parent_session_id: Some("parent".into()),
        });

        let event = receiver.recv().await.unwrap();
        assert_eq!(event.session_id, "child");
        assert_eq!(event.parent_session_id.as_deref(), Some("parent"));
        assert!(matches!(event.event, AgentEvent::SessionStarted {
            provider: Some(_),
            model:    Some(_),
        }));
    }
}
