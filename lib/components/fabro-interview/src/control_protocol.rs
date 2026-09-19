use std::time::Duration;

use fabro_types::{PairId, PairMessageId, PairTarget, Principal, RunId};
use serde::{Deserialize, Serialize};

use crate::{Answer, AnswerSubmission, AnswerValue};

pub const WORKER_CONTROL_PROTOCOL_VERSION: u8 = 1;

/// Interval between worker-control WebSocket ping frames.
///
/// Server and worker both initiate pings at this cadence; either side that
/// fails to observe inbound traffic for [`WORKER_CONTROL_WS_LIVENESS_TIMEOUT`]
/// closes the WebSocket.
pub const WORKER_CONTROL_WS_PING_INTERVAL: Duration = Duration::from_secs(15);

/// Maximum quiet time allowed on a worker-control WebSocket before either side
/// declares the connection dead.
pub const WORKER_CONTROL_WS_LIVENESS_TIMEOUT: Duration = Duration::from_secs(45);

/// WebSocket close-frame reason used when the server can no longer prove
/// replay correctness for the requested cursor. Workers must treat this as
/// fatal control-channel loss.
pub const WORKER_CONTROL_INVALID_CURSOR_REASON: &str = "invalid_cursor";

/// WebSocket close-frame reason used when the ping/pong watchdog fires.
pub const WORKER_CONTROL_PONG_TIMEOUT_REASON: &str = "pong_timeout";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerControlEnvelope {
    pub v:       u8,
    #[serde(flatten)]
    pub message: WorkerControlMessage,
}

impl WorkerControlEnvelope {
    #[must_use]
    pub fn interview_answer(qid: impl Into<String>, submission: AnswerSubmission) -> Self {
        Self {
            v:       WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::InterviewAnswer {
                qid:    qid.into(),
                answer: submission.answer.into(),
                actor:  submission.actor,
            },
        }
    }

    #[must_use]
    pub fn cancel_run() -> Self {
        Self {
            v:       WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::RunCancel,
        }
    }

    #[must_use]
    pub fn pause_run() -> Self {
        Self {
            v:       WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::RunPause,
        }
    }

    #[must_use]
    pub fn unpause_run() -> Self {
        Self {
            v:       WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::RunUnpause,
        }
    }

    #[must_use]
    pub fn steer(text: impl Into<String>, stage: Option<String>, actor: Principal) -> Self {
        Self {
            v:       WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::Steer {
                text: text.into(),
                stage,
                actor,
                request_id: None,
            },
        }
    }

    #[must_use]
    pub fn interrupt(stage: Option<String>, actor: Principal) -> Self {
        Self {
            v:       WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::Interrupt {
                stage,
                actor,
                request_id: None,
            },
        }
    }

    #[must_use]
    pub fn interrupt_then_steer(
        text: impl Into<String>,
        stage: Option<String>,
        actor: Principal,
    ) -> Self {
        Self {
            v:       WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::InterruptThenSteer {
                text: text.into(),
                stage,
                actor,
                request_id: None,
            },
        }
    }

    /// The same control, asking the worker to acknowledge it: the worker
    /// answers a control that carries a request id with a
    /// [`WorkerControlAck`] naming the id, over the control stream it
    /// arrived on. Only a steer or an interrupt carries one; on any other
    /// control the id is dropped.
    #[must_use]
    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        match &mut self.message {
            WorkerControlMessage::Steer { request_id, .. }
            | WorkerControlMessage::Interrupt { request_id, .. }
            | WorkerControlMessage::InterruptThenSteer { request_id, .. } => {
                *request_id = Some(id.into());
            }
            _ => {}
        }
        self
    }

    /// The request id the control carries, when its sender asked for an
    /// acknowledgement.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match &self.message {
            WorkerControlMessage::Steer { request_id, .. }
            | WorkerControlMessage::Interrupt { request_id, .. }
            | WorkerControlMessage::InterruptThenSteer { request_id, .. } => request_id.as_deref(),
            _ => None,
        }
    }

    #[must_use]
    pub fn start_pair(
        run_id: RunId,
        pair_id: PairId,
        target: PairTarget,
        actor: Principal,
    ) -> Self {
        Self {
            v:       WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::PairStart {
                run_id,
                pair_id,
                target,
                actor,
            },
        }
    }

    #[must_use]
    pub fn pair_message(
        pair_id: PairId,
        message_id: PairMessageId,
        text: impl Into<String>,
        client_message_id: Option<String>,
        actor: Principal,
    ) -> Self {
        Self {
            v:       WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::PairMessage {
                pair_id,
                message_id,
                text: text.into(),
                client_message_id,
                actor,
            },
        }
    }

    #[must_use]
    pub fn end_pair(pair_id: PairId, actor: Principal) -> Self {
        Self {
            v:       WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::PairEnd { pair_id, actor },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WorkerControlMessage {
    #[serde(rename = "interview.answer")]
    InterviewAnswer {
        qid:    String,
        answer: WorkerControlAnswer,
        actor:  Principal,
    },
    #[serde(rename = "run.cancel")]
    RunCancel,
    #[serde(rename = "run.pause")]
    RunPause,
    #[serde(rename = "run.unpause")]
    RunUnpause,
    #[serde(rename = "run.steer")]
    Steer {
        text:       String,
        /// The stage to steer (`node@visit`, or the node name); `None`
        /// steers the run's one live agent stage.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage:      Option<String>,
        actor:      Principal,
        /// Set when the sender waits for a [`WorkerControlAck`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    #[serde(rename = "run.interrupt")]
    Interrupt {
        /// The stage whose model turn to stop (`node@visit`, or the node
        /// name); `None` interrupts the run's one live agent stage.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage:      Option<String>,
        actor:      Principal,
        /// Set when the sender waits for a [`WorkerControlAck`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    #[serde(rename = "run.interrupt_then_steer")]
    InterruptThenSteer {
        text:       String,
        /// The stage to interrupt and steer, as for `Interrupt`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage:      Option<String>,
        actor:      Principal,
        /// Set when the sender waits for a [`WorkerControlAck`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    #[serde(rename = "pair.start")]
    PairStart {
        run_id:  RunId,
        pair_id: PairId,
        target:  PairTarget,
        actor:   Principal,
    },
    #[serde(rename = "pair.message")]
    PairMessage {
        pair_id:           PairId,
        message_id:        PairMessageId,
        text:              String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_message_id: Option<String>,
        actor:             Principal,
    },
    #[serde(rename = "pair.end")]
    PairEnd { pair_id: PairId, actor: Principal },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerControlDeliveryFrame {
    pub id:       String,
    pub envelope: WorkerControlEnvelope,
}

/// The worker's answer to a control that carried a request id, sent as a
/// text frame over the control stream the control arrived on: what
/// became of it, so the server can answer the caller in its own response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerControlAck {
    pub v:          u8,
    pub request_id: String,
    pub outcome:    WorkerControlOutcome,
}

impl WorkerControlAck {
    #[must_use]
    pub fn new(request_id: impl Into<String>, outcome: WorkerControlOutcome) -> Self {
        Self {
            v: WORKER_CONTROL_PROTOCOL_VERSION,
            request_id: request_id.into(),
            outcome,
        }
    }
}

/// What became of a control at the worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerControlOutcome {
    /// The control reached its stage: the label of the stage it went to,
    /// when the control had one.
    Delivered {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage: Option<String>,
    },
    /// The worker or Petri refused the control: the code the refusal is
    /// known by (`no_live_turn`, `no_such_stage`, `steer_refused`,
    /// `interrupt_refused`) and the reason as the worker spells it.
    Refused { code: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerControlAnswer {
    Yes,
    No,
    Cancelled,
    Interrupted,
    Skipped,
    Timeout,
    Selected { key: String },
    MultiSelected { keys: Vec<String> },
    Text { text: String },
}

impl From<Answer> for WorkerControlAnswer {
    fn from(answer: Answer) -> Self {
        match answer.value {
            AnswerValue::Yes => Self::Yes,
            AnswerValue::No => Self::No,
            AnswerValue::Cancelled => Self::Cancelled,
            AnswerValue::Interrupted => Self::Interrupted,
            AnswerValue::Skipped => Self::Skipped,
            AnswerValue::Timeout => Self::Timeout,
            AnswerValue::Selected(key) => Self::Selected { key },
            AnswerValue::MultiSelected(keys) => Self::MultiSelected { keys },
            AnswerValue::Text(text) => Self::Text { text },
        }
    }
}

impl From<WorkerControlAnswer> for Answer {
    fn from(answer: WorkerControlAnswer) -> Self {
        match answer {
            WorkerControlAnswer::Yes => Self::yes(),
            WorkerControlAnswer::No => Self::no(),
            WorkerControlAnswer::Cancelled => Self::cancelled(),
            WorkerControlAnswer::Interrupted => Self::interrupted(),
            WorkerControlAnswer::Skipped => Self::skipped(),
            WorkerControlAnswer::Timeout => Self::timeout(),
            WorkerControlAnswer::Selected { key } => Self {
                value:           AnswerValue::Selected(key),
                selected_option: None,
                text:            None,
            },
            WorkerControlAnswer::MultiSelected { keys } => Self::multi_selected(keys),
            WorkerControlAnswer::Text { text } => Self::text(text),
        }
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::{PairTarget, Principal, SystemActorKind, fixtures};

    use super::*;

    #[test]
    fn interview_answer_round_trips_through_json() {
        let envelope = WorkerControlEnvelope::interview_answer(
            "q-1",
            AnswerSubmission::system(Answer::text("ship it"), SystemActorKind::Engine),
        );
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"type":"interview.answer","qid":"q-1","answer":{"kind":"text","text":"ship it"},"actor":{"kind":"system","system_kind":"engine"}}"#
        );

        let parsed: WorkerControlEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, envelope);
    }

    #[test]
    fn cancel_run_round_trips_through_json() {
        let envelope = WorkerControlEnvelope::cancel_run();
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(json, r#"{"v":1,"type":"run.cancel"}"#);

        let parsed: WorkerControlEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, envelope);
    }

    #[test]
    fn pause_run_round_trips_through_json() {
        let envelope = WorkerControlEnvelope::pause_run();
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(json, r#"{"v":1,"type":"run.pause"}"#);

        let parsed: WorkerControlEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, envelope);
    }

    #[test]
    fn unpause_run_round_trips_through_json() {
        let envelope = WorkerControlEnvelope::unpause_run();
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(json, r#"{"v":1,"type":"run.unpause"}"#);

        let parsed: WorkerControlEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, envelope);
    }

    #[test]
    fn steer_append_round_trips_through_json() {
        let envelope = WorkerControlEnvelope::steer("try again", None, Principal::System {
            system_kind: SystemActorKind::Engine,
        });
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"type":"run.steer","text":"try again","actor":{"kind":"system","system_kind":"engine"}}"#
        );
        let parsed: WorkerControlEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, envelope);
    }

    #[test]
    fn interrupt_round_trips_through_json() {
        let envelope = WorkerControlEnvelope::interrupt(None, Principal::System {
            system_kind: SystemActorKind::Engine,
        });
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"type":"run.interrupt","actor":{"kind":"system","system_kind":"engine"}}"#
        );
        let parsed: WorkerControlEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, envelope);
    }

    #[test]
    fn interrupt_then_steer_round_trips_through_json() {
        let envelope = WorkerControlEnvelope::interrupt_then_steer(
            "stop, do X instead",
            Some("code@2".to_string()),
            Principal::System {
                system_kind: SystemActorKind::Engine,
            },
        );
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"type":"run.interrupt_then_steer","text":"stop, do X instead","stage":"code@2","actor":{"kind":"system","system_kind":"engine"}}"#
        );
        let parsed: WorkerControlEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, envelope);
    }

    #[test]
    fn pair_start_round_trips_through_json() {
        let envelope = WorkerControlEnvelope::start_pair(
            fixtures::RUN_1,
            "01HZX6M29F1CD5YYMHT1F5D7WQ".parse().unwrap(),
            PairTarget {
                stage_id:   "code@1".parse().unwrap(),
                node_label: "Code".to_string(),
            },
            Principal::System {
                system_kind: SystemActorKind::Engine,
            },
        );
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: WorkerControlEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, envelope);

        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let target = &value["target"];
        let target_text = target.to_string();
        assert!(target_text.contains("stage_id"));
        assert!(target_text.contains("node_label"));
        assert!(!target_text.contains("agent_session_id"));
        assert!(!target_text.contains("session_id"));
        assert!(!target_text.contains("\"node_id\""));
        assert!(!target_text.contains("\"visit\""));
        assert!(!target_text.contains("provider"));
        assert!(!target_text.contains("model"));
    }

    #[test]
    fn pair_message_and_end_round_trip_through_json() {
        let actor = Principal::System {
            system_kind: SystemActorKind::Engine,
        };
        let pair_id = "01HZX6M29F1CD5YYMHT1F5D7WQ".parse().unwrap();
        let message_id = "01HZX6M4D7Y1QW0Q0P6V8Z4DR5".parse().unwrap();

        let message = WorkerControlEnvelope::pair_message(
            pair_id,
            message_id,
            "continue here",
            Some("client-1".to_string()),
            actor.clone(),
        );
        let parsed: WorkerControlEnvelope =
            serde_json::from_str(&serde_json::to_string(&message).unwrap()).unwrap();
        assert_eq!(parsed, message);

        let end = WorkerControlEnvelope::end_pair(pair_id, actor);
        let parsed: WorkerControlEnvelope =
            serde_json::from_str(&serde_json::to_string(&end).unwrap()).unwrap();
        assert_eq!(parsed, end);
    }

    #[test]
    fn a_request_id_rides_a_steer_or_an_interrupt_and_nothing_else() {
        let actor = Principal::System {
            system_kind: SystemActorKind::Engine,
        };
        let steer =
            WorkerControlEnvelope::steer("try again", None, actor.clone()).with_request_id("req-1");
        assert_eq!(steer.request_id(), Some("req-1"));
        let json = serde_json::to_string(&steer).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"type":"run.steer","text":"try again","actor":{"kind":"system","system_kind":"engine"},"request_id":"req-1"}"#
        );
        let parsed: WorkerControlEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, steer);

        let interrupt = WorkerControlEnvelope::interrupt(Some("code@2".to_string()), actor.clone())
            .with_request_id("req-2");
        assert_eq!(interrupt.request_id(), Some("req-2"));
        let interrupt_then_steer = WorkerControlEnvelope::interrupt_then_steer("stop", None, actor)
            .with_request_id("req-3");
        assert_eq!(interrupt_then_steer.request_id(), Some("req-3"));

        let pause = WorkerControlEnvelope::pause_run().with_request_id("req-4");
        assert_eq!(pause.request_id(), None);
        assert_eq!(pause, WorkerControlEnvelope::pause_run());
    }

    #[test]
    fn a_steer_without_a_request_id_still_parses() {
        let parsed: WorkerControlEnvelope = serde_json::from_str(
            r#"{"v":1,"type":"run.steer","text":"try again","actor":{"kind":"system","system_kind":"engine"}}"#,
        )
        .unwrap();
        assert_eq!(parsed.request_id(), None);
    }

    #[test]
    fn control_acks_round_trip_through_json() {
        let delivered = WorkerControlAck::new("req-1", WorkerControlOutcome::Delivered {
            stage: Some("work@1".to_string()),
        });
        let json = serde_json::to_string(&delivered).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"request_id":"req-1","outcome":{"kind":"delivered","stage":"work@1"}}"#
        );
        let parsed: WorkerControlAck = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, delivered);

        let refused = WorkerControlAck::new("req-2", WorkerControlOutcome::Refused {
            code:    "no_live_turn".to_string(),
            message: "the stage has no model turn to interrupt".to_string(),
        });
        let json = serde_json::to_string(&refused).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"request_id":"req-2","outcome":{"kind":"refused","code":"no_live_turn","message":"the stage has no model turn to interrupt"}}"#
        );
        let parsed: WorkerControlAck = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, refused);
    }

    #[test]
    fn delivery_frame_round_trips_through_json() {
        let frame = WorkerControlDeliveryFrame {
            id:       "local:42".to_string(),
            envelope: WorkerControlEnvelope::cancel_run(),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(
            json,
            r#"{"id":"local:42","envelope":{"v":1,"type":"run.cancel"}}"#
        );

        let parsed: WorkerControlDeliveryFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, frame);
    }
}
