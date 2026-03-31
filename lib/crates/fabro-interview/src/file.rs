use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio::fs;
use tokio::time;

use crate::{Answer, Interviewer, Question};

#[cfg(test)]
use std::path::Path;

/// An interviewer that communicates via JSON files in the runtime directory.
///
/// The engine process writes `interview_request.json` and polls for
/// `interview_response.json`. The attach process watches for the request
/// file, prompts the user, and writes the response file.
#[allow(clippy::struct_field_names)]
pub struct FileInterviewer {
    request_path: PathBuf,
    response_path: PathBuf,
    claim_path: PathBuf,
    poll_interval: Duration,
    reattach_window: Duration,
}

#[cfg(test)]
const DEFAULT_REATTACH_WINDOW: Duration = Duration::from_millis(300);
#[cfg(not(test))]
const DEFAULT_REATTACH_WINDOW: Duration = Duration::from_secs(30);

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[cfg(test)]
const TEST_POLL_INTERVAL: Duration = Duration::from_millis(1);
#[cfg(test)]
const TEST_REATTACH_WINDOW: Duration = Duration::from_millis(5);

fn default_reattach_window() -> Duration {
    DEFAULT_REATTACH_WINDOW
}

fn default_poll_interval() -> Duration {
    DEFAULT_POLL_INTERVAL
}

impl FileInterviewer {
    pub fn new(request_path: PathBuf, response_path: PathBuf, claim_path: PathBuf) -> Self {
        Self {
            request_path,
            response_path,
            claim_path,
            poll_interval: default_poll_interval(),
            reattach_window: default_reattach_window(),
        }
    }

    #[cfg(test)]
    fn with_timing(
        request_path: PathBuf,
        response_path: PathBuf,
        claim_path: PathBuf,
        poll_interval: Duration,
        reattach_window: Duration,
    ) -> Self {
        Self {
            request_path,
            response_path,
            claim_path,
            poll_interval,
            reattach_window,
        }
    }

    fn request_path(&self) -> PathBuf {
        self.request_path.clone()
    }

    fn response_path(&self) -> PathBuf {
        self.response_path.clone()
    }

    fn claim_path(&self) -> PathBuf {
        self.claim_path.clone()
    }

    async fn write_request_atomically(&self, question: &Question) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(question).expect("Question serialization failed");
        let request_path = self.request_path();
        if let Some(parent) = request_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let temp_path = request_path.with_extension("json.tmp");
        fs::write(&temp_path, json).await?;
        fs::rename(temp_path, request_path).await
    }

    async fn cleanup_ipc_files(&self) {
        let _ = fs::remove_file(self.request_path()).await;
        let _ = fs::remove_file(self.response_path()).await;
        let _ = fs::remove_file(self.claim_path()).await;
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
            let mut reattach_deadline: Option<time::Instant> = None;
            loop {
                match fs::read_to_string(&response_path).await {
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
                    reattach_deadline = Some(time::Instant::now() + self.reattach_window);
                }

                if let Some(deadline) = reattach_deadline {
                    if time::Instant::now() >= deadline {
                        self.cleanup_ipc_files().await;
                        return default_for_claim_timeout.unwrap_or_else(Answer::timeout);
                    }
                }

                time::sleep(self.poll_interval).await;
            }
        };

        if let Some(secs) = timeout_secs {
            let duration = std::time::Duration::from_secs_f64(secs);
            if let Ok(answer) = time::timeout(duration, poll).await {
                answer
            } else {
                self.cleanup_ipc_files().await;
                default_answer.unwrap_or_else(Answer::timeout)
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

    fn test_interviewer(
        request_path: PathBuf,
        response_path: PathBuf,
        claim_path: PathBuf,
    ) -> FileInterviewer {
        FileInterviewer::with_timing(
            request_path,
            response_path,
            claim_path,
            TEST_POLL_INTERVAL,
            TEST_REATTACH_WINDOW,
        )
    }

    fn interviewer_paths(run_dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let runtime_dir = run_dir.join("runtime");
        (
            runtime_dir.join("interview_request.json"),
            runtime_dir.join("interview_response.json"),
            runtime_dir.join("interview_request.claim"),
        )
    }

    async fn wait_for_exists(path: &Path) {
        for _ in 0..200 {
            if path.exists() {
                return;
            }
            time::sleep(TEST_POLL_INTERVAL).await;
        }
        panic!("{} should exist", path.display());
    }

    #[tokio::test]
    async fn write_request_poll_response() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().to_path_buf();
        let (request_path, response_path, claim_path) = interviewer_paths(&run_dir);
        let interviewer = test_interviewer(
            request_path.clone(),
            response_path.clone(),
            claim_path.clone(),
        );

        let question = Question::new("approve?", QuestionType::YesNo);

        // Spawn the ask in a background task
        let ask_handle = tokio::spawn(async move { interviewer.ask(question).await });

        // Wait for the request file to appear
        wait_for_exists(&request_path).await;

        // Verify the request contains valid Question JSON
        let request_data = fs::read_to_string(&request_path).await.unwrap();
        let parsed: Question = serde_json::from_str(&request_data).unwrap();
        assert_eq!(parsed.text, "approve?");

        // Write a response
        let answer = Answer::yes();
        let response_json = serde_json::to_string_pretty(&answer).unwrap();
        fs::write(&response_path, response_json).await.unwrap();

        // Wait for the ask to complete
        let result = ask_handle.await.unwrap();
        assert_eq!(result.value, AnswerValue::Yes);

        // Both files should be cleaned up
        assert!(!request_path.exists());
        assert!(!response_path.exists());
        assert!(!claim_path.exists());
    }

    #[tokio::test]
    async fn timeout_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let (request_path, response_path, claim_path) = interviewer_paths(dir.path());
        let interviewer = test_interviewer(request_path, response_path, claim_path);

        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.timeout_seconds = Some(0.02);
        question.default = Some(Answer::no());

        let answer = interviewer.ask(question).await;
        assert_eq!(answer.value, AnswerValue::No);
    }

    #[tokio::test]
    async fn claim_released_without_response_returns_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().to_path_buf();
        let (request_path, response_path, claim_path) = interviewer_paths(&run_dir);
        let interviewer = test_interviewer(request_path.clone(), response_path, claim_path.clone());

        let question = Question::new("approve?", QuestionType::YesNo);

        let ask_handle = tokio::spawn(async move { interviewer.ask(question).await });

        // Wait for request file to appear
        wait_for_exists(&request_path).await;

        // Simulate attacher creating claim file
        std::fs::write(&claim_path, "12345\n").unwrap();

        // Let the poll loop see the claim
        time::sleep(TEST_POLL_INTERVAL * 2).await;

        // Simulate attacher departing (deletes claim without writing response)
        std::fs::remove_file(&claim_path).unwrap();

        // Should return timeout within REATTACH_WINDOW
        let started = time::Instant::now();
        let answer = time::timeout(Duration::from_millis(250), ask_handle)
            .await
            .expect("should complete quickly")
            .unwrap();

        assert_eq!(answer.value, AnswerValue::Timeout);
        assert!(
            started.elapsed() <= Duration::from_millis(100),
            "should resolve well within the reattach window"
        );
    }

    #[tokio::test]
    async fn claim_released_without_response_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().to_path_buf();
        let (request_path, response_path, claim_path) = interviewer_paths(&run_dir);
        let interviewer = test_interviewer(request_path.clone(), response_path, claim_path.clone());

        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.default = Some(Answer::no());

        let ask_handle = tokio::spawn(async move { interviewer.ask(question).await });

        // Wait for request file
        wait_for_exists(&request_path).await;

        // Simulate attacher creating then deleting claim
        std::fs::write(&claim_path, "12345\n").unwrap();
        time::sleep(TEST_POLL_INTERVAL * 2).await;
        std::fs::remove_file(&claim_path).unwrap();

        let answer = time::timeout(Duration::from_millis(250), ask_handle)
            .await
            .expect("should complete quickly")
            .unwrap();

        assert_eq!(answer.value, AnswerValue::No);
    }

    #[tokio::test]
    async fn claim_released_then_new_attacher_answers() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().to_path_buf();
        let (request_path, response_path, claim_path) = interviewer_paths(&run_dir);
        let interviewer = test_interviewer(
            request_path.clone(),
            response_path.clone(),
            claim_path.clone(),
        );

        let question = Question::new("approve?", QuestionType::YesNo);

        let ask_handle = tokio::spawn(async move { interviewer.ask(question).await });

        // Wait for request file
        wait_for_exists(&request_path).await;

        // First attacher creates then releases claim
        std::fs::write(&claim_path, "12345\n").unwrap();
        time::sleep(TEST_POLL_INTERVAL * 2).await;
        std::fs::remove_file(&claim_path).unwrap();

        // Second attacher picks up and answers before reattach window expires
        time::sleep(TEST_POLL_INTERVAL * 2).await;
        std::fs::write(&claim_path, "12346\n").unwrap();

        let answer = Answer::yes();
        let response_json = serde_json::to_string_pretty(&answer).unwrap();
        fs::write(response_path, response_json).await.unwrap();

        let result = time::timeout(Duration::from_millis(250), ask_handle)
            .await
            .expect("should complete quickly")
            .unwrap();

        assert_eq!(result.value, AnswerValue::Yes);
    }

    #[tokio::test]
    async fn timeout_without_default_returns_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let (request_path, response_path, claim_path) = interviewer_paths(dir.path());
        let interviewer = test_interviewer(request_path, response_path, claim_path);

        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.timeout_seconds = Some(0.02);

        let answer = interviewer.ask(question).await;
        assert_eq!(answer.value, AnswerValue::Timeout);
    }
}
