mod auto_approve;
mod callback;
mod console;
mod control;
mod control_protocol;
mod queue;
mod recording;
mod replay;

use std::collections::HashMap;

use async_trait::async_trait;
use fabro_types::{InterviewOption, Principal, QuestionType, SystemActorKind};
use serde::{Deserialize, Serialize};
use tokio::time;

/// A question presented to the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    #[serde(default)]
    pub id:              String,
    pub text:            String,
    pub question_type:   QuestionType,
    pub options:         Vec<InterviewOption>,
    pub allow_freeform:  bool,
    pub default:         Option<Answer>,
    pub timeout_seconds: Option<f64>,
    pub stage:           String,
    pub metadata:        HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub context_display: Option<String>,
}

impl Question {
    pub fn new(text: impl Into<String>, question_type: QuestionType) -> Self {
        Self {
            id: String::new(),
            text: text.into(),
            question_type,
            options: Vec::new(),
            allow_freeform: false,
            default: None,
            timeout_seconds: None,
            stage: String::new(),
            metadata: HashMap::new(),
            context_display: None,
        }
    }
}

/// The value of an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnswerValue {
    Yes,
    No,
    Cancelled,
    Interrupted,
    Skipped,
    Timeout,
    Selected(String),
    MultiSelected(Vec<String>),
    Text(String),
}

/// An answer from the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Answer {
    pub value:           AnswerValue,
    pub selected_option: Option<InterviewOption>,
    pub text:            Option<String>,
}

impl Answer {
    #[must_use]
    pub fn yes() -> Self {
        Self {
            value:           AnswerValue::Yes,
            selected_option: None,
            text:            None,
        }
    }

    #[must_use]
    pub fn no() -> Self {
        Self {
            value:           AnswerValue::No,
            selected_option: None,
            text:            None,
        }
    }

    #[must_use]
    pub fn cancelled() -> Self {
        Self {
            value:           AnswerValue::Cancelled,
            selected_option: None,
            text:            None,
        }
    }

    #[must_use]
    pub fn interrupted() -> Self {
        Self {
            value:           AnswerValue::Interrupted,
            selected_option: None,
            text:            None,
        }
    }

    #[must_use]
    pub fn skipped() -> Self {
        Self {
            value:           AnswerValue::Skipped,
            selected_option: None,
            text:            None,
        }
    }

    #[must_use]
    pub fn timeout() -> Self {
        Self {
            value:           AnswerValue::Timeout,
            selected_option: None,
            text:            None,
        }
    }

    pub fn selected(key: impl Into<String>, option: InterviewOption) -> Self {
        let key = key.into();
        Self {
            value:           AnswerValue::Selected(key),
            selected_option: Some(option),
            text:            None,
        }
    }

    pub fn multi_selected(keys: Vec<String>) -> Self {
        Self {
            value:           AnswerValue::MultiSelected(keys),
            selected_option: None,
            text:            None,
        }
    }

    pub fn text(text: impl Into<String>) -> Self {
        let t = text.into();
        Self {
            value:           AnswerValue::Text(t.clone()),
            selected_option: None,
            text:            Some(t),
        }
    }
}

/// An answer plus the principal that supplied it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerSubmission {
    pub answer: Answer,
    pub actor:  Principal,
}

impl AnswerSubmission {
    #[must_use]
    pub fn new(answer: Answer, actor: Principal) -> Self {
        Self { answer, actor }
    }

    #[must_use]
    pub fn system(answer: Answer, system_kind: SystemActorKind) -> Self {
        Self {
            answer,
            actor: Principal::System { system_kind },
        }
    }
}

/// Apply timeout enforcement to an interviewer ask call.
/// Per spec 6.5: if `timeout_seconds` is set, returns default answer or
/// `Answer::timeout()`.
pub async fn ask_with_timeout(
    interviewer: &dyn Interviewer,
    question: Question,
) -> AnswerSubmission {
    let timeout_secs = question.timeout_seconds;
    let default_answer = question.default.clone();

    if let Some(secs) = timeout_secs {
        let duration = std::time::Duration::from_secs_f64(secs);
        match time::timeout(duration, interviewer.ask(question)).await {
            Ok(answer) => answer,
            Err(_elapsed) => AnswerSubmission::system(
                default_answer.unwrap_or_else(Answer::timeout),
                SystemActorKind::Timeout,
            ),
        }
    } else {
        interviewer.ask(question).await
    }
}

/// The interviewer trait for human-in-the-loop interactions.
#[async_trait]
pub trait Interviewer: Send + Sync {
    async fn ask(&self, question: Question) -> AnswerSubmission;

    async fn ask_multiple(&self, questions: Vec<Question>) -> Vec<AnswerSubmission> {
        let mut answers = Vec::with_capacity(questions.len());
        for q in questions {
            answers.push(self.ask(q).await);
        }
        answers
    }

    async fn inform(&self, _message: &str, _stage: &str) {
        // Default no-op
    }
}

// Re-export all implementors at the crate root
pub use auto_approve::AutoApproveInterviewer;
pub use callback::CallbackInterviewer;
pub use console::ConsoleInterviewer;
pub use control::{ControlInterviewer, SubmitError};
pub use control_protocol::{
    WORKER_CONTROL_PROTOCOL_VERSION, WorkerControlAnswer, WorkerControlEnvelope,
    WorkerControlMessage,
};
pub use queue::QueueInterviewer;
pub use recording::RecordingInterviewer;
pub use replay::ReplayInterviewer;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::time;

    use super::*;

    #[test]
    fn question_type_display() {
        assert_eq!(QuestionType::YesNo.to_string(), "yes_no");
        assert_eq!(QuestionType::MultipleChoice.to_string(), "multiple_choice");
        assert_eq!(QuestionType::MultiSelect.to_string(), "multi_select");
        assert_eq!(QuestionType::Freeform.to_string(), "freeform");
        assert_eq!(QuestionType::Confirmation.to_string(), "confirmation");
    }

    #[test]
    fn question_new() {
        let q = Question::new("Do you approve?", QuestionType::YesNo);
        assert!(q.id.is_empty());
        assert_eq!(q.text, "Do you approve?");
        assert_eq!(q.question_type, QuestionType::YesNo);
        assert!(q.options.is_empty());
        assert!(!q.allow_freeform);
        assert!(q.default.is_none());
        assert!(q.timeout_seconds.is_none());
        assert!(q.stage.is_empty());
        assert!(q.metadata.is_empty());
    }

    #[test]
    fn answer_yes() {
        let a = Answer::yes();
        assert_eq!(a.value, AnswerValue::Yes);
        assert!(a.selected_option.is_none());
        assert!(a.text.is_none());
    }

    #[test]
    fn answer_no() {
        let a = Answer::no();
        assert_eq!(a.value, AnswerValue::No);
    }

    #[test]
    fn answer_cancelled() {
        let a = Answer::cancelled();
        assert_eq!(a.value, AnswerValue::Cancelled);
    }

    #[test]
    fn answer_skipped() {
        let a = Answer::skipped();
        assert_eq!(a.value, AnswerValue::Skipped);
    }

    #[test]
    fn answer_interrupted() {
        let a = Answer::interrupted();
        assert_eq!(a.value, AnswerValue::Interrupted);
    }

    #[test]
    fn answer_timeout() {
        let a = Answer::timeout();
        assert_eq!(a.value, AnswerValue::Timeout);
    }

    #[test]
    fn answer_selected() {
        let opt = InterviewOption {
            key:   "A".to_string(),
            label: "Approve".to_string(),
        };
        let a = Answer::selected("A", opt.clone());
        assert_eq!(a.value, AnswerValue::Selected("A".to_string()));
        assert_eq!(a.selected_option, Some(opt));
    }

    #[test]
    fn answer_text() {
        let a = Answer::text("free input");
        assert_eq!(a.value, AnswerValue::Text("free input".to_string()));
        assert_eq!(a.text, Some("free input".to_string()));
    }

    #[test]
    fn question_option_eq() {
        let a = InterviewOption {
            key:   "Y".to_string(),
            label: "Yes".to_string(),
        };
        let b = InterviewOption {
            key:   "Y".to_string(),
            label: "Yes".to_string(),
        };
        assert_eq!(a, b);
    }

    #[test]
    fn answer_value_variants() {
        assert_ne!(AnswerValue::Yes, AnswerValue::No);
        assert_ne!(AnswerValue::Cancelled, AnswerValue::Interrupted);
        assert_ne!(AnswerValue::Skipped, AnswerValue::Timeout);
        assert_ne!(AnswerValue::Interrupted, AnswerValue::Timeout);
        assert_eq!(
            AnswerValue::Selected("x".to_string()),
            AnswerValue::Selected("x".to_string())
        );
        assert_eq!(
            AnswerValue::Text("hello".to_string()),
            AnswerValue::Text("hello".to_string())
        );
    }

    #[test]
    fn question_type_multi_select_exists() {
        let q = Question::new("Pick many:", QuestionType::MultiSelect);
        assert_eq!(q.question_type, QuestionType::MultiSelect);
    }

    /// A slow interviewer that waits before answering -- for testing timeouts.
    struct SlowInterviewer;

    #[async_trait]
    impl Interviewer for SlowInterviewer {
        async fn ask(&self, _question: Question) -> AnswerSubmission {
            time::sleep(std::time::Duration::from_mins(1)).await;
            AnswerSubmission::system(Answer::yes(), SystemActorKind::Engine)
        }
    }

    #[tokio::test]
    async fn ask_with_timeout_returns_timeout_when_expired() {
        let interviewer = SlowInterviewer;
        let mut q = Question::new("approve?", QuestionType::YesNo);
        q.timeout_seconds = Some(0.01);

        let answer = ask_with_timeout(&interviewer, q).await.answer;
        assert_eq!(answer.value, AnswerValue::Timeout);
    }

    #[tokio::test]
    async fn ask_with_timeout_returns_default_when_set() {
        let interviewer = SlowInterviewer;
        let mut q = Question::new("approve?", QuestionType::YesNo);
        q.timeout_seconds = Some(0.01);
        q.default = Some(Answer::no());

        let answer = ask_with_timeout(&interviewer, q).await.answer;
        assert_eq!(answer.value, AnswerValue::No);
    }

    #[tokio::test]
    async fn ask_with_timeout_no_timeout_returns_normally() {
        let interviewer = AutoApproveInterviewer::engine();
        let q = Question::new("approve?", QuestionType::YesNo);

        let answer = ask_with_timeout(&interviewer, q).await.answer;
        assert_eq!(answer.value, AnswerValue::Yes);
    }

    #[tokio::test]
    async fn control_interviewer_routes_answers_by_question_id() {
        let interviewer = Arc::new(ControlInterviewer::new());

        let mut question = Question::new("Approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();

        let ask_interviewer = Arc::clone(&interviewer);
        let ask = tokio::spawn(async move { ask_interviewer.ask(question).await });

        time::sleep(std::time::Duration::from_millis(10)).await;
        interviewer
            .submit(
                "q-1",
                AnswerSubmission::system(Answer::yes(), SystemActorKind::Engine),
            )
            .await
            .unwrap();

        let answer = ask.await.unwrap().answer;
        assert_eq!(answer.value, AnswerValue::Yes);
    }
}
