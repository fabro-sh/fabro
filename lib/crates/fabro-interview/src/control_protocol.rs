use fabro_types::{PairId, PairMessageId, PairTarget, Principal, RunId};
use serde::{Deserialize, Serialize};

use crate::{Answer, AnswerSubmission, AnswerValue};

pub const WORKER_CONTROL_PROTOCOL_VERSION: u8 = 1;

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
    pub fn steer(text: impl Into<String>, actor: Principal) -> Self {
        Self {
            v:       WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::Steer {
                text: text.into(),
                actor,
            },
        }
    }

    #[must_use]
    pub fn interrupt(actor: Principal) -> Self {
        Self {
            v:       WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::Interrupt { actor },
        }
    }

    #[must_use]
    pub fn interrupt_then_steer(text: impl Into<String>, actor: Principal) -> Self {
        Self {
            v:       WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::InterruptThenSteer {
                text: text.into(),
                actor,
            },
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
    #[serde(rename = "run.steer")]
    Steer { text: String, actor: Principal },
    #[serde(rename = "run.interrupt")]
    Interrupt { actor: Principal },
    #[serde(rename = "run.interrupt_then_steer")]
    InterruptThenSteer { text: String, actor: Principal },
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
    fn steer_append_round_trips_through_json() {
        let envelope = WorkerControlEnvelope::steer("try again", Principal::System {
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
        let envelope = WorkerControlEnvelope::interrupt(Principal::System {
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
        let envelope =
            WorkerControlEnvelope::interrupt_then_steer("stop, do X instead", Principal::System {
                system_kind: SystemActorKind::Engine,
            });
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"type":"run.interrupt_then_steer","text":"stop, do X instead","actor":{"kind":"system","system_kind":"engine"}}"#
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
}
