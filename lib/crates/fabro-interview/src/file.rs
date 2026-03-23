use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;

use crate::{Answer, Interviewer, Question};

#[cfg(test)]
const REATTACH_WINDOW: Duration = Duration::from_millis(300);
#[cfg(not(test))]
const REATTACH_WINDOW: Duration = Duration::from_secs(30);

/// An interviewer that communicates via JSON files in the run directory.
///
/// The engine process writes `interview_request.json` and polls for
/// `interview_response.json`. The attach process watches for the request
/// file, prompts the user, and writes the response file.
pub struct FileInterviewer {
    run_dir: PathBuf,
}

impl FileInterviewer {
    pub fn new(run_dir: PathBuf) -> Self {
        Self { run_dir }
    }

    fn request_path(&self) -> PathBuf {
        self.run_dir.join("interview_request.json")
    }

    fn response_path(&self) -> PathBuf {
        self.run_dir.join("interview_response.json")
    }

    fn claim_path(&self) -> PathBuf {
        self.run_dir.join("interview_request.claim")
    }

    async fn write_request_atomically(&self, question: &Question) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(question).expect("Question serialization failed");
        let request_path = self.request_path();
        let temp_path = request_path.with_extension("json.tmp");
        tokio::fs::write(&temp_path, json).await?;
        tokio::fs::rename(temp_path, request_path).await
    }

    async fn cleanup_ipc_files(&self) {
        let _ = tokio::fs::remove_file(self.request_path()).await;
        let _ = tokio::fs::remove_file(self.response_path()).await;
        let _ = tokio::fs::remove_file(self.claim_path()).await;
    }
}

#[async_trait]
impl Interviewer for FileInterviewer {
    async fn ask(&self, question: Question) -> Answer {
        let timeout_secs = question.timeout_seconds;
        let default_answer = question.default.clone();

        // Write the request file
        if let Err(e) = self.write_request_atomically(&question).await {
            tracing::warn!(error = %e, "Failed to write interview request");
            return default_answer.unwrap_or_else(Answer::timeout);
        }

        // Poll for response with optional timeout
        let default_for_claim_timeout = default_answer.clone();
        let poll = async {
            let response_path = self.response_path();
            let claim_path = self.claim_path();
            let mut claim_was_seen = false;
            let mut reattach_deadline: Option<tokio::time::Instant> = None;
            loop {
                match tokio::fs::read_to_string(&response_path).await {
                    Ok(data) => match serde_json::from_str::<Answer>(&data) {
                        Ok(answer) => {
                            self.cleanup_ipc_files().await;
                            return answer;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to parse interview response, retrying");
                            // File might be partially written, wait and retry
                        }
                    },
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        // Not written yet — check claim state below
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to read interview response, retrying");
                    }
                }

                // Monitor claim file to detect attacher departure
                if claim_path.exists() {
                    claim_was_seen = true;
                    reattach_deadline = None;
                } else if claim_was_seen && reattach_deadline.is_none() {
                    reattach_deadline = Some(tokio::time::Instant::now() + REATTACH_WINDOW);
                }

                if let Some(deadline) = reattach_deadline {
                    if tokio::time::Instant::now() >= deadline {
                        self.cleanup_ipc_files().await;
                        return default_for_claim_timeout.unwrap_or_else(Answer::timeout);
                    }
                }

                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };

        if let Some(secs) = timeout_secs {
            let duration = std::time::Duration::from_secs_f64(secs);
            match tokio::time::timeout(duration, poll).await {
                Ok(answer) => answer,
                Err(_) => {
                    self.cleanup_ipc_files().await;
                    default_answer.unwrap_or_else(Answer::timeout)
                }
            }
        } else {
            poll.await
        }
    }

    async fn inform(&self, _message: &str, _stage: &str) {
        // No-op: inform messages are rendered by the attach process via progress.jsonl
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnswerValue, QuestionType};

    #[tokio::test]
    async fn write_request_poll_response() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().to_path_buf();
        let interviewer = FileInterviewer::new(run_dir.clone());

        let question = Question::new("approve?", QuestionType::YesNo);

        // Spawn the ask in a background task
        let ask_handle = tokio::spawn(async move { interviewer.ask(question).await });

        // Wait for the request file to appear
        let request_path = run_dir.join("interview_request.json");
        for _ in 0..50 {
            if request_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(request_path.exists(), "interview_request.json should exist");

        // Verify the request contains valid Question JSON
        let request_data = tokio::fs::read_to_string(&request_path).await.unwrap();
        let parsed: Question = serde_json::from_str(&request_data).unwrap();
        assert_eq!(parsed.text, "approve?");

        // Write a response
        let answer = Answer::yes();
        let response_json = serde_json::to_string_pretty(&answer).unwrap();
        let response_path = run_dir.join("interview_response.json");
        tokio::fs::write(&response_path, response_json)
            .await
            .unwrap();

        // Wait for the ask to complete
        let result = ask_handle.await.unwrap();
        assert_eq!(result.value, AnswerValue::Yes);

        // Both files should be cleaned up
        assert!(!request_path.exists());
        assert!(!response_path.exists());
        assert!(!run_dir.join("interview_request.claim").exists());
    }

    #[tokio::test]
    async fn timeout_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let interviewer = FileInterviewer::new(dir.path().to_path_buf());

        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.timeout_seconds = Some(0.1);
        question.default = Some(Answer::no());

        let answer = interviewer.ask(question).await;
        assert_eq!(answer.value, AnswerValue::No);
    }

    #[tokio::test]
    async fn claim_released_without_response_returns_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().to_path_buf();
        let interviewer = FileInterviewer::new(run_dir.clone());

        let question = Question::new("approve?", QuestionType::YesNo);

        let ask_handle = tokio::spawn(async move { interviewer.ask(question).await });

        // Wait for request file to appear
        let request_path = run_dir.join("interview_request.json");
        for _ in 0..50 {
            if request_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(request_path.exists());

        // Simulate attacher creating claim file
        let claim_path = run_dir.join("interview_request.claim");
        std::fs::write(&claim_path, "12345\n").unwrap();

        // Let the poll loop see the claim
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Simulate attacher departing (deletes claim without writing response)
        std::fs::remove_file(&claim_path).unwrap();

        // Should return timeout within REATTACH_WINDOW
        let started = tokio::time::Instant::now();
        let answer = tokio::time::timeout(Duration::from_secs(2), ask_handle)
            .await
            .expect("should complete within 2s")
            .unwrap();

        assert_eq!(answer.value, AnswerValue::Timeout);
        assert!(
            started.elapsed() <= Duration::from_secs(1),
            "should resolve well within the reattach window"
        );
    }

    #[tokio::test]
    async fn claim_released_without_response_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().to_path_buf();
        let interviewer = FileInterviewer::new(run_dir.clone());

        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.default = Some(Answer::no());

        let ask_handle = tokio::spawn(async move { interviewer.ask(question).await });

        // Wait for request file
        let request_path = run_dir.join("interview_request.json");
        for _ in 0..50 {
            if request_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(request_path.exists());

        // Simulate attacher creating then deleting claim
        let claim_path = run_dir.join("interview_request.claim");
        std::fs::write(&claim_path, "12345\n").unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        std::fs::remove_file(&claim_path).unwrap();

        let answer = tokio::time::timeout(Duration::from_secs(2), ask_handle)
            .await
            .expect("should complete within 2s")
            .unwrap();

        assert_eq!(answer.value, AnswerValue::No);
    }

    #[tokio::test]
    async fn claim_released_then_new_attacher_answers() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().to_path_buf();
        let interviewer = FileInterviewer::new(run_dir.clone());

        let question = Question::new("approve?", QuestionType::YesNo);

        let run_dir2 = run_dir.clone();
        let ask_handle = tokio::spawn(async move { interviewer.ask(question).await });

        // Wait for request file
        let request_path = run_dir.join("interview_request.json");
        for _ in 0..50 {
            if request_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(request_path.exists());

        // First attacher creates then releases claim
        let claim_path = run_dir.join("interview_request.claim");
        std::fs::write(&claim_path, "12345\n").unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        std::fs::remove_file(&claim_path).unwrap();

        // Second attacher picks up and answers before reattach window expires
        tokio::time::sleep(Duration::from_millis(50)).await;
        std::fs::write(&claim_path, "12346\n").unwrap();

        let answer = Answer::yes();
        let response_json = serde_json::to_string_pretty(&answer).unwrap();
        tokio::fs::write(run_dir2.join("interview_response.json"), response_json)
            .await
            .unwrap();

        let result = tokio::time::timeout(Duration::from_secs(2), ask_handle)
            .await
            .expect("should complete within 2s")
            .unwrap();

        assert_eq!(result.value, AnswerValue::Yes);
    }

    #[tokio::test]
    async fn timeout_without_default_returns_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let interviewer = FileInterviewer::new(dir.path().to_path_buf());

        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.timeout_seconds = Some(0.1);

        let answer = interviewer.ask(question).await;
        assert_eq!(answer.value, AnswerValue::Timeout);
    }
}
