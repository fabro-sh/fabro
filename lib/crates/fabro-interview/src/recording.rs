use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::{AnswerSubmission, Interviewer, Question};

/// Wraps another interviewer and records all question-answer pairs.
pub struct RecordingInterviewer {
    inner:       Box<dyn Interviewer>,
    submissions: Mutex<Vec<(Question, AnswerSubmission)>>,
}

impl RecordingInterviewer {
    #[must_use]
    pub fn new(inner: Box<dyn Interviewer>) -> Self {
        Self {
            inner,
            submissions: Mutex::new(Vec::new()),
        }
    }

    /// # Panics
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn recordings(&self) -> Vec<(Question, AnswerSubmission)> {
        self.submissions
            .lock()
            .expect("recordings lock poisoned")
            .clone()
    }

    /// Serializes all recordings to a JSON string.
    ///
    /// # Errors
    /// Returns an error if serialization fails.
    pub fn to_json(&self) -> std::io::Result<String> {
        let recordings = self.recordings();
        serde_json::to_string_pretty(&recordings).map_err(std::io::Error::other)
    }

    /// Deserializes recordings from a JSON string.
    ///
    /// # Errors
    /// Returns an error if deserialization fails.
    pub fn from_json(json: &str) -> std::io::Result<Vec<(Question, AnswerSubmission)>> {
        serde_json::from_str(json).map_err(std::io::Error::other)
    }

    /// Saves recordings to a file as JSON.
    ///
    /// # Errors
    /// Returns an error if serialization or file writing fails.
    #[expect(
        clippy::disallowed_methods,
        reason = "sync helper for test-mode interview recording storage; not on a Tokio path"
    )]
    pub fn save_to_file(&self, path: &Path) -> std::io::Result<()> {
        let json = self.to_json()?;
        std::fs::write(path, json).map_err(|err| {
            std::io::Error::new(
                err.kind(),
                format!("write interview recording {}: {err}", path.display()),
            )
        })?;
        Ok(())
    }

    /// Loads recordings from a JSON file.
    ///
    /// # Errors
    /// Returns an error if file reading or deserialization fails.
    #[expect(
        clippy::disallowed_methods,
        reason = "sync helper for test-mode interview recording storage; not on a Tokio path"
    )]
    pub fn load_from_file(path: &Path) -> std::io::Result<Vec<(Question, AnswerSubmission)>> {
        let json = std::fs::read_to_string(path).map_err(|err| {
            std::io::Error::new(
                err.kind(),
                format!("read interview recording {}: {err}", path.display()),
            )
        })?;
        Self::from_json(&json)
    }
}

#[async_trait]
impl Interviewer for RecordingInterviewer {
    async fn ask(&self, question: Question) -> AnswerSubmission {
        let submission = self.inner.ask(question.clone()).await;
        self.submissions
            .lock()
            .expect("recordings lock poisoned")
            .push((question, submission.clone()));
        submission
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::QuestionType;

    use super::*;
    use crate::{AnswerValue, AutoApproveInterviewer};

    #[tokio::test]
    async fn records_question_answer_pairs() {
        let inner = Box::new(AutoApproveInterviewer::engine());
        let recorder = RecordingInterviewer::new(inner);

        let q1 = Question::new("approve?", QuestionType::YesNo);
        let q2 = Question::new("confirm?", QuestionType::Confirmation);

        let a1 = recorder.ask(q1).await.answer;
        assert_eq!(a1.value, AnswerValue::Yes);

        let a2 = recorder.ask(q2).await.answer;
        assert_eq!(a2.value, AnswerValue::Yes);

        let recs = recorder.recordings();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].0.text, "approve?");
        assert_eq!(recs[1].0.text, "confirm?");
    }

    #[tokio::test]
    async fn delegates_to_inner() {
        let inner = Box::new(AutoApproveInterviewer::engine());
        let recorder = RecordingInterviewer::new(inner);

        let q = Question::new("text input", QuestionType::Freeform);
        let answer = recorder.ask(q).await.answer;
        assert_eq!(answer.value, AnswerValue::Text("auto-approved".to_string()));
    }

    #[tokio::test]
    async fn recordings_empty_initially() {
        let inner = Box::new(AutoApproveInterviewer::engine());
        let recorder = RecordingInterviewer::new(inner);
        assert!(recorder.recordings().is_empty());
    }

    #[tokio::test]
    async fn to_json_serializes_recordings() {
        let inner = Box::new(AutoApproveInterviewer::engine());
        let recorder = RecordingInterviewer::new(inner);

        let q = Question::new("approve?", QuestionType::YesNo);
        recorder.ask(q).await;

        let json = recorder.to_json().unwrap();
        assert!(json.contains("approve?"));
        assert!(json.contains("yes_no"));
    }

    #[test]
    fn from_json_deserializes_recordings() {
        let json = r#"[
            [
                {"text":"approve?","question_type":"yes_no","options":[],"allow_freeform":false,"default":null,"timeout_seconds":null,"stage":"","metadata":{}},
                {
                    "answer":{"value":"Yes","selected_option":null,"text":null},
                    "actor":{"kind":"system","system_kind":"engine"}
                }
            ]
        ]"#;

        let recordings = RecordingInterviewer::from_json(json).unwrap();
        assert_eq!(recordings.len(), 1);
        assert_eq!(recordings[0].0.text, "approve?");
        assert_eq!(recordings[0].1.answer.value, AnswerValue::Yes);
    }

    #[tokio::test]
    async fn save_to_file_and_load_from_file() {
        let inner = Box::new(AutoApproveInterviewer::engine());
        let recorder = RecordingInterviewer::new(inner);

        let q = Question::new("approve?", QuestionType::YesNo);
        recorder.ask(q).await;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recordings.json");

        recorder.save_to_file(&path).unwrap();
        let loaded = RecordingInterviewer::load_from_file(&path).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0.text, "approve?");
        assert_eq!(loaded[0].1.answer.value, AnswerValue::Yes);
    }

    #[tokio::test]
    async fn round_trip_serialize_deserialize() {
        let inner = Box::new(AutoApproveInterviewer::engine());
        let recorder = RecordingInterviewer::new(inner);

        let q1 = Question::new("approve?", QuestionType::YesNo);
        let q2 = Question::new("confirm?", QuestionType::Confirmation);
        recorder.ask(q1).await;
        recorder.ask(q2).await;

        let json = recorder.to_json().unwrap();
        let restored = RecordingInterviewer::from_json(&json).unwrap();

        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].0.text, "approve?");
        assert_eq!(restored[0].0.question_type, QuestionType::YesNo);
        assert_eq!(restored[0].1.answer.value, AnswerValue::Yes);
        assert_eq!(restored[1].0.text, "confirm?");
        assert_eq!(restored[1].0.question_type, QuestionType::Confirmation);
        assert_eq!(restored[1].1.answer.value, AnswerValue::Yes);
    }
}
