use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::oneshot;

use crate::{Answer, Interviewer, Question};

/// A pending question waiting for an answer from an external source (e.g., HTTP endpoint).
#[derive(Debug)]
pub struct PendingQuestion {
    pub id: String,
    pub question: Question,
}

/// Internal state: maps question ID to its oneshot sender.
struct WebInterviewerInner {
    pending: HashMap<String, oneshot::Sender<Answer>>,
    questions: Vec<PendingQuestion>,
    next_id: u64,
}

/// An interviewer that holds questions until answers are submitted externally.
///
/// When `ask()` is called, the question is enqueued with a unique ID and the call
/// blocks until `submit_answer()` is called with the matching ID.
pub struct WebInterviewer {
    inner: Arc<Mutex<WebInterviewerInner>>,
}

impl WebInterviewer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(WebInterviewerInner {
                pending: HashMap::new(),
                questions: Vec::new(),
                next_id: 1,
            })),
        }
    }

    /// Returns a snapshot of currently pending questions.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned.
    #[must_use]
    pub fn pending_questions(&self) -> Vec<PendingQuestion> {
        let inner = self.inner.lock().expect("web interviewer lock poisoned");
        inner
            .questions
            .iter()
            .map(|pq| PendingQuestion {
                id: pq.id.clone(),
                question: pq.question.clone(),
            })
            .collect()
    }

    /// Submit an answer for a pending question by ID.
    /// Returns `true` if the question was found and the answer was delivered,
    /// `false` if no such question was pending.
    ///
    /// # Panics
    ///
    /// Panics if the internal lock is poisoned.
    #[must_use]
    pub fn submit_answer(&self, question_id: &str, answer: Answer) -> bool {
        let sender = {
            let mut inner = self.inner.lock().expect("web interviewer lock poisoned");
            let sender = inner.pending.remove(question_id);
            if sender.is_some() {
                inner.questions.retain(|pq| pq.id != question_id);
            }
            sender
        };
        sender.is_some_and(|tx| tx.send(answer).is_ok())
    }
}

impl Default for WebInterviewer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Interviewer for WebInterviewer {
    async fn ask(&self, question: Question) -> Answer {
        let (tx, rx) = oneshot::channel();

        {
            let mut inner = self.inner.lock().expect("web interviewer lock poisoned");
            let id = format!("q-{}", inner.next_id);
            inner.next_id += 1;
            inner.pending.insert(id.clone(), tx);
            inner.questions.push(PendingQuestion {
                id,
                question: question.clone(),
            });
        }

        // Block until answer arrives or sender is dropped
        rx.await.unwrap_or_else(|_| Answer::aborted())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnswerValue, QuestionType};
    use std::sync::Arc;

    #[tokio::test]
    async fn ask_blocks_until_answer_submitted() {
        let interviewer = Arc::new(WebInterviewer::new());
        let interviewer_clone = Arc::clone(&interviewer);

        let ask_handle = tokio::spawn(async move {
            let q = Question::new("approve?", QuestionType::YesNo);
            interviewer_clone.ask(q).await
        });

        // Give the ask task a moment to register the question
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Question should be pending
        let pending = interviewer.pending_questions();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].question.text, "approve?");

        // Submit answer
        let submitted = interviewer.submit_answer(&pending[0].id, Answer::yes());
        assert!(submitted);

        // ask() should now return
        let answer = ask_handle.await.expect("task should complete");
        assert_eq!(answer.value, AnswerValue::Yes);
    }

    #[tokio::test]
    async fn submit_answer_unblocks_ask() {
        let interviewer = Arc::new(WebInterviewer::new());
        let interviewer_clone = Arc::clone(&interviewer);

        let ask_handle = tokio::spawn(async move {
            let q = Question::new("name?", QuestionType::Freeform);
            interviewer_clone.ask(q).await
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let pending = interviewer.pending_questions();
        assert_eq!(pending.len(), 1);

        let _ = interviewer.submit_answer(&pending[0].id, Answer::text("Alice"));

        let answer = ask_handle.await.expect("task should complete");
        assert_eq!(answer.value, AnswerValue::Text("Alice".to_string()));
        assert_eq!(answer.text, Some("Alice".to_string()));
    }

    #[tokio::test]
    async fn timeout_returns_default_or_timeout_answer() {
        let interviewer = Arc::new(WebInterviewer::new());

        let mut q = Question::new("approve?", QuestionType::YesNo);
        q.timeout_seconds = Some(0.05);

        // Use ask_with_timeout from the parent module
        let answer = crate::ask_with_timeout(interviewer.as_ref(), q).await;
        assert_eq!(answer.value, AnswerValue::Timeout);
    }

    #[tokio::test]
    async fn question_id_correlation() {
        let interviewer = Arc::new(WebInterviewer::new());
        let i1 = Arc::clone(&interviewer);
        let i2 = Arc::clone(&interviewer);

        // Spawn two concurrent asks
        let handle1 = tokio::spawn(async move {
            let q = Question::new("first?", QuestionType::YesNo);
            i1.ask(q).await
        });

        let handle2 = tokio::spawn(async move {
            let q = Question::new("second?", QuestionType::YesNo);
            i2.ask(q).await
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let pending = interviewer.pending_questions();
        assert_eq!(pending.len(), 2);

        // Find which ID corresponds to which question
        let first_id = pending
            .iter()
            .find(|pq| pq.question.text == "first?")
            .expect("first question should be pending")
            .id
            .clone();
        let second_id = pending
            .iter()
            .find(|pq| pq.question.text == "second?")
            .expect("second question should be pending")
            .id
            .clone();

        // Answer them in reverse order
        let _ = interviewer.submit_answer(&second_id, Answer::no());
        let _ = interviewer.submit_answer(&first_id, Answer::yes());

        let answer1 = handle1.await.expect("task should complete");
        let answer2 = handle2.await.expect("task should complete");

        assert_eq!(answer1.value, AnswerValue::Yes);
        assert_eq!(answer2.value, AnswerValue::No);
    }

    #[test]
    fn submit_answer_for_unknown_id_returns_false() {
        let interviewer = WebInterviewer::new();
        let result = interviewer.submit_answer("nonexistent", Answer::yes());
        assert!(!result);
    }

    #[tokio::test]
    async fn pending_questions_empty_initially() {
        let interviewer = WebInterviewer::new();
        assert!(interviewer.pending_questions().is_empty());
    }

    #[tokio::test]
    async fn pending_questions_cleared_after_answer() {
        let interviewer = Arc::new(WebInterviewer::new());
        let i_clone = Arc::clone(&interviewer);

        let handle = tokio::spawn(async move {
            let q = Question::new("q?", QuestionType::YesNo);
            i_clone.ask(q).await
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let pending = interviewer.pending_questions();
        assert_eq!(pending.len(), 1);

        let _ = interviewer.submit_answer(&pending[0].id, Answer::yes());
        handle.await.expect("task should complete");

        assert!(interviewer.pending_questions().is_empty());
    }

    #[tokio::test]
    async fn ask_returns_aborted_when_pending_sender_is_dropped() {
        let interviewer = Arc::new(WebInterviewer::new());
        let interviewer_clone = Arc::clone(&interviewer);

        let ask_handle = tokio::spawn(async move {
            let q = Question::new("approve?", QuestionType::YesNo);
            interviewer_clone.ask(q).await
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        {
            let mut inner = interviewer
                .inner
                .lock()
                .expect("web interviewer lock poisoned");
            let pending_id = inner
                .questions
                .first()
                .expect("question should be pending")
                .id
                .clone();
            inner.pending.remove(&pending_id);
            inner.questions.retain(|pq| pq.id != pending_id);
        }

        let answer = ask_handle.await.expect("task should complete");
        assert_eq!(answer.value, AnswerValue::Aborted);
    }
}
