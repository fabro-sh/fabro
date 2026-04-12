use fabro_interview::Answer;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlackQuestionRef {
    pub run_id: String,
    pub qid:    String,
}

#[derive(Debug, Clone)]
pub struct SlackAnswerSubmission {
    pub run_id: String,
    pub qid:    String,
    pub answer: Answer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SlackActionPayload {
    Yes {
        run_id: String,
        qid:    String,
    },
    No {
        run_id: String,
        qid:    String,
    },
    Selected {
        run_id: String,
        qid:    String,
        key:    String,
    },
    SubmitMulti {
        run_id: String,
        qid:    String,
    },
}

impl SlackActionPayload {
    #[must_use]
    pub fn question_ref(&self) -> SlackQuestionRef {
        match self {
            Self::Yes { run_id, qid }
            | Self::No { run_id, qid }
            | Self::Selected { run_id, qid, .. }
            | Self::SubmitMulti { run_id, qid } => SlackQuestionRef {
                run_id: run_id.clone(),
                qid:    qid.clone(),
            },
        }
    }
}

#[must_use]
pub fn encode_action_value(payload: &SlackActionPayload) -> String {
    serde_json::to_string(payload).expect("Slack action payload serialization should succeed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_payload_serializes_run_id_and_qid() {
        let payload = SlackActionPayload::Selected {
            run_id: "run_123".to_string(),
            qid:    "q_123".to_string(),
            key:    "approve".to_string(),
        };
        let json = encode_action_value(&payload);
        assert_eq!(
            json,
            r#"{"kind":"selected","run_id":"run_123","qid":"q_123","key":"approve"}"#
        );
    }
}
