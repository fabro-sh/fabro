use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::{Mutex, oneshot};

use crate::{Answer, Interviewer, Question};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    AlreadyResolved,
}

#[derive(Default)]
struct ControlInterviewerState {
    pending: HashMap<String, oneshot::Sender<Answer>>,
    queued: HashMap<String, Answer>,
    closed: bool,
}

#[derive(Default)]
pub struct ControlInterviewer {
    state: Mutex<ControlInterviewerState>,
}

impl ControlInterviewer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    async fn register(&self, question_id: String) -> oneshot::Receiver<Answer> {
        let mut state = self.state.lock().await;
        if state.closed {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Answer::aborted());
            return rx;
        }
        if let Some(answer) = state.queued.remove(&question_id) {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(answer);
            return rx;
        }

        let (tx, rx) = oneshot::channel();
        state.pending.insert(question_id, tx);
        rx
    }

    pub async fn submit(&self, question_id: &str, answer: Answer) -> Result<(), SubmitError> {
        let pending_sender = {
            let mut state = self.state.lock().await;
            if state.closed {
                return Err(SubmitError::AlreadyResolved);
            }
            if let Some(sender) = state.pending.remove(question_id) {
                Some(sender)
            } else if state.queued.contains_key(question_id) {
                return Err(SubmitError::AlreadyResolved);
            } else {
                state.queued.insert(question_id.to_string(), answer);
                return Ok(());
            }
        };

        match pending_sender {
            Some(sender) => sender
                .send(answer)
                .map_err(|_| SubmitError::AlreadyResolved),
            None => Err(SubmitError::AlreadyResolved),
        }
    }

    pub async fn abort_all(&self) {
        let (pending, queued) = {
            let mut state = self.state.lock().await;
            state.closed = true;
            let pending = state
                .pending
                .drain()
                .map(|(_, sender)| sender)
                .collect::<Vec<_>>();
            let queued = state.queued.len();
            state.queued.clear();
            (pending, queued)
        };

        for sender in pending {
            let _ = sender.send(Answer::aborted());
        }

        if queued > 0 {
            tracing::debug!(
                count = queued,
                "Dropped queued interview answers while aborting control interviewer"
            );
        }
    }
}

#[async_trait]
impl Interviewer for ControlInterviewer {
    async fn ask(&self, question: Question) -> Answer {
        let receiver = self.register(question.id.clone()).await;
        match receiver.await {
            Ok(answer) => answer,
            Err(_) => Answer::aborted(),
        }
    }

    async fn inform(&self, _message: &str, _stage: &str) {
        // No-op: progress rendering happens via run events.
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{AnswerValue, QuestionType};

    use super::*;

    #[tokio::test]
    async fn submit_before_ask_buffers_answer() {
        let interviewer = ControlInterviewer::new();
        let result = interviewer.submit("q-1", Answer::yes()).await;
        assert_eq!(result, Ok(()));
    }

    #[tokio::test]
    async fn register_then_submit_delivers_answer() {
        let interviewer = Arc::new(ControlInterviewer::new());

        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();

        let ask_interviewer = Arc::clone(&interviewer);
        let ask = tokio::spawn(async move { ask_interviewer.ask(question).await });
        let submit_result = interviewer.submit("q-1", Answer::yes()).await;

        assert_eq!(submit_result, Ok(()));
        let answer = ask.await.unwrap();
        assert_eq!(answer.value, AnswerValue::Yes);
    }

    #[tokio::test]
    async fn submit_before_register_buffers_answer() {
        let interviewer = Arc::new(ControlInterviewer::new());
        assert_eq!(interviewer.submit("q-1", Answer::no()).await, Ok(()));

        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();
        let answer = interviewer.ask(question).await;
        assert_eq!(answer.value, AnswerValue::No);
    }

    #[tokio::test]
    async fn duplicate_buffered_answer_is_rejected() {
        let interviewer = ControlInterviewer::new();
        assert_eq!(interviewer.submit("q-1", Answer::yes()).await, Ok(()));
        assert_eq!(
            interviewer.submit("q-1", Answer::no()).await,
            Err(SubmitError::AlreadyResolved)
        );
    }

    #[tokio::test]
    async fn abort_all_aborts_pending_questions() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();

        let ask_interviewer = Arc::clone(&interviewer);
        let ask = tokio::spawn(async move { ask_interviewer.ask(question).await });
        tokio::task::yield_now().await;

        interviewer.abort_all().await;

        let answer = ask.await.unwrap();
        assert_eq!(answer.value, AnswerValue::Aborted);
    }

    #[tokio::test]
    async fn ask_after_abort_all_returns_aborted() {
        let interviewer = ControlInterviewer::new();
        interviewer.abort_all().await;

        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();

        let answer = interviewer.ask(question).await;
        assert_eq!(answer.value, AnswerValue::Aborted);
    }
}
