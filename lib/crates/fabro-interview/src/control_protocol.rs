use fabro_types::Principal;
pub use fabro_types::SteerKind;
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
    pub fn steer(text: impl Into<String>, kind: SteerKind, actor: Principal) -> Self {
        Self {
            v:       WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::Steer {
                text: text.into(),
                kind,
                actor,
            },
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
    Steer {
        text:  String,
        kind:  SteerKind,
        actor: Principal,
    },
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
    use super::*;

    #[test]
    fn interview_answer_round_trips_through_json() {
        let envelope = WorkerControlEnvelope::interview_answer(
            "q-1",
            AnswerSubmission::system(
                Answer::text("ship it"),
                fabro_types::SystemActorKind::Engine,
            ),
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
        let envelope = WorkerControlEnvelope::steer(
            "try again",
            SteerKind::Append,
            fabro_types::Principal::System {
                system_kind: fabro_types::SystemActorKind::Engine,
            },
        );
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"type":"run.steer","text":"try again","kind":"append","actor":{"kind":"system","system_kind":"engine"}}"#
        );
        let parsed: WorkerControlEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, envelope);
    }

    #[test]
    fn steer_interrupt_round_trips_through_json() {
        let envelope = WorkerControlEnvelope::steer(
            "stop, do X instead",
            SteerKind::Interrupt,
            fabro_types::Principal::System {
                system_kind: fabro_types::SystemActorKind::Engine,
            },
        );
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(json.contains(r#""kind":"interrupt""#));
        let parsed: WorkerControlEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, envelope);
    }
}
