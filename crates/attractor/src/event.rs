use serde::{Deserialize, Serialize};

use crate::outcome::StageUsage;

/// Events emitted during pipeline execution for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PipelineEvent {
    PipelineStarted {
        name: String,
        id: String,
    },
    PipelineCompleted {
        duration_ms: u64,
        artifact_count: usize,
    },
    PipelineFailed {
        error: String,
        duration_ms: u64,
    },
    StageStarted {
        name: String,
        index: usize,
    },
    StageCompleted {
        name: String,
        index: usize,
        duration_ms: u64,
        status: String,
        preferred_label: Option<String>,
        suggested_next_ids: Vec<String>,
        usage: Option<StageUsage>,
    },
    StageFailed {
        name: String,
        index: usize,
        error: String,
        will_retry: bool,
    },
    StageRetrying {
        name: String,
        index: usize,
        attempt: usize,
        delay_ms: u64,
    },
    ParallelStarted {
        branch_count: usize,
    },
    ParallelBranchStarted {
        branch: String,
        index: usize,
    },
    ParallelBranchCompleted {
        branch: String,
        index: usize,
        duration_ms: u64,
        success: bool,
    },
    ParallelCompleted {
        duration_ms: u64,
        success_count: usize,
        failure_count: usize,
    },
    InterviewStarted {
        question: String,
        stage: String,
    },
    InterviewCompleted {
        question: String,
        answer: String,
        duration_ms: u64,
    },
    InterviewTimeout {
        question: String,
        stage: String,
        duration_ms: u64,
    },
    CheckpointSaved {
        node_id: String,
    },
}

/// Listener callback type for pipeline events.
type EventListener = Box<dyn Fn(&PipelineEvent) + Send + Sync>;

/// Callback-based event emitter for pipeline events.
pub struct EventEmitter {
    listeners: Vec<EventListener>,
}

impl std::fmt::Debug for EventEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventEmitter")
            .field("listener_count", &self.listeners.len())
            .finish()
    }
}

impl Default for EventEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl EventEmitter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            listeners: Vec::new(),
        }
    }

    pub fn on_event(&mut self, listener: impl Fn(&PipelineEvent) + Send + Sync + 'static) {
        self.listeners.push(Box::new(listener));
    }

    pub fn emit(&self, event: &PipelineEvent) {
        for listener in &self.listeners {
            listener(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn event_emitter_new_has_no_listeners() {
        let emitter = EventEmitter::new();
        assert_eq!(emitter.listeners.len(), 0);
    }

    #[test]
    fn event_emitter_calls_listener() {
        let mut emitter = EventEmitter::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        emitter.on_event(move |event| {
            let name = match event {
                PipelineEvent::PipelineStarted { name, .. } => name.clone(),
                _ => "other".to_string(),
            };
            received_clone.lock().unwrap().push(name);
        });
        emitter.emit(&PipelineEvent::PipelineStarted {
            name: "test".to_string(),
            id: "1".to_string(),
        });
        let events = received.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], "test");
    }

    #[test]
    fn pipeline_event_serialization() {
        let event = PipelineEvent::StageStarted {
            name: "plan".to_string(),
            index: 0,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("StageStarted"));
        assert!(json.contains("plan"));
    }

    #[test]
    fn event_emitter_default() {
        let emitter = EventEmitter::default();
        assert_eq!(emitter.listeners.len(), 0);
    }
}
