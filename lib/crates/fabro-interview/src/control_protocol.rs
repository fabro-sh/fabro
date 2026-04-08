use serde::{Deserialize, Serialize};

use crate::{Answer, AnswerValue};

pub const WORKER_CONTROL_PROTOCOL_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerControlEnvelope {
    pub v: u8,
    #[serde(flatten)]
    pub message: WorkerControlMessage,
}

impl WorkerControlEnvelope {
    #[must_use]
    pub fn interview_answer(qid: impl Into<String>, answer: Answer) -> Self {
        Self {
            v: WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::InterviewAnswer {
                qid: qid.into(),
                answer: answer.into(),
            },
        }
    }

    #[must_use]
    pub fn cancel_run() -> Self {
        Self {
            v: WORKER_CONTROL_PROTOCOL_VERSION,
            message: WorkerControlMessage::RunCancel,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WorkerControlMessage {
    #[serde(rename = "interview.answer")]
    InterviewAnswer {
        qid: String,
        answer: WorkerControlAnswer,
    },
    #[serde(rename = "run.cancel")]
    RunCancel,
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
                value: AnswerValue::Selected(key),
                selected_option: None,
                text: None,
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
        let envelope = WorkerControlEnvelope::interview_answer("q-1", Answer::text("ship it"));
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"type":"interview.answer","qid":"q-1","answer":{"kind":"text","text":"ship it"}}"#
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
}
