use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::{Mutex, oneshot};

use crate::{Answer, AnswerSubmission, Interviewer, Question};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    AlreadyResolved,
}

#[derive(Default)]
struct ControlInterviewerState {
    pending:             HashMap<String, oneshot::Sender<AnswerSubmission>>,
    queued:              HashMap<String, AnswerSubmission>,
    terminal_submission: Option<AnswerSubmission>,
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

    async fn register(&self, question_id: String) -> oneshot::Receiver<AnswerSubmission> {
        let mut state = self.state.lock().await;
        if let Some(submission) = state.terminal_submission.clone() {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(submission);
            return rx;
        }
        if let Some(submission) = state.queued.remove(&question_id) {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(submission);
            return rx;
        }

        let (tx, rx) = oneshot::channel();
        state.pending.insert(question_id, tx);
        rx
    }

    pub async fn submit(
        &self,
        question_id: &str,
        submission: AnswerSubmission,
    ) -> Result<(), SubmitError> {
        let pending_sender = {
            let mut state = self.state.lock().await;
            if state.terminal_submission.is_some() {
                return Err(SubmitError::AlreadyResolved);
            }
            if let Some(sender) = state.pending.remove(question_id) {
                Some(sender)
            } else if state.queued.contains_key(question_id) {
                return Err(SubmitError::AlreadyResolved);
            } else {
                state.queued.insert(question_id.to_string(), submission);
                return Ok(());
            }
        };

        match pending_sender {
            Some(sender) => sender
                .send(submission)
                .map_err(|_| SubmitError::AlreadyResolved),
            None => Err(SubmitError::AlreadyResolved),
        }
    }

    pub async fn interrupt_all(&self) {
        self.resolve_all(AnswerSubmission::system(
            Answer::interrupted(),
            fabro_types::SystemActorKind::Engine,
        ))
        .await;
    }

    pub async fn cancel_all(&self) {
        self.resolve_all(AnswerSubmission::system(
            Answer::cancelled(),
            fabro_types::SystemActorKind::Engine,
        ))
        .await;
    }

    async fn resolve_all(&self, submission: AnswerSubmission) {
        let (pending, queued) = {
            let mut state = self.state.lock().await;
            state.terminal_submission = Some(submission.clone());
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
            let _ = sender.send(submission.clone());
        }

        if queued > 0 {
            tracing::debug!(
                count = queued,
                "Dropped queued interview answers while interrupting control interviewer"
            );
        }
    }
}

#[async_trait]
impl Interviewer for ControlInterviewer {
    async fn ask(&self, question: Question) -> AnswerSubmission {
        let receiver = self.register(question.id.clone()).await;
        match receiver.await {
            Ok(submission) => submission,
            Err(_) => AnswerSubmission::system(
                Answer::interrupted(),
                fabro_types::SystemActorKind::Engine,
            ),
        }
    }

    async fn inform(&self, _message: &str, _stage: &str) {
        // No-op: progress rendering happens via run events.
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fabro_types::QuestionType;
    use tokio::task;

    use super::*;
    use crate::AnswerValue;

    fn submission(answer: Answer) -> AnswerSubmission {
        AnswerSubmission::system(answer, fabro_types::SystemActorKind::Engine)
    }

    #[tokio::test]
    async fn submit_before_ask_buffers_answer() {
        let interviewer = ControlInterviewer::new();
        let result = interviewer.submit("q-1", submission(Answer::yes())).await;
        assert_eq!(result, Ok(()));
    }

    #[tokio::test]
    async fn register_then_submit_delivers_answer() {
        let interviewer = Arc::new(ControlInterviewer::new());

        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();

        let ask_interviewer = Arc::clone(&interviewer);
        let ask = tokio::spawn(async move { ask_interviewer.ask(question).await });
        let submit_result = interviewer.submit("q-1", submission(Answer::yes())).await;

        assert_eq!(submit_result, Ok(()));
        let answer = ask.await.unwrap().answer;
        assert_eq!(answer.value, AnswerValue::Yes);
    }

    #[tokio::test]
    async fn submit_before_register_buffers_answer() {
        let interviewer = Arc::new(ControlInterviewer::new());
        assert_eq!(
            interviewer.submit("q-1", submission(Answer::no())).await,
            Ok(())
        );

        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();
        let answer = interviewer.ask(question).await.answer;
        assert_eq!(answer.value, AnswerValue::No);
    }

    #[tokio::test]
    async fn duplicate_buffered_answer_is_rejected() {
        let interviewer = ControlInterviewer::new();
        assert_eq!(
            interviewer.submit("q-1", submission(Answer::yes())).await,
            Ok(())
        );
        assert_eq!(
            interviewer.submit("q-1", submission(Answer::no())).await,
            Err(SubmitError::AlreadyResolved)
        );
    }

    #[tokio::test]
    async fn interrupt_all_interrupts_pending_questions() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();

        let ask_interviewer = Arc::clone(&interviewer);
        let ask = tokio::spawn(async move { ask_interviewer.ask(question).await });
        task::yield_now().await;

        interviewer.interrupt_all().await;

        let answer = ask.await.unwrap().answer;
        assert_eq!(answer.value, AnswerValue::Interrupted);
    }

    #[tokio::test]
    async fn ask_after_interrupt_all_returns_interrupted() {
        let interviewer = ControlInterviewer::new();
        interviewer.interrupt_all().await;

        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();

        let answer = interviewer.ask(question).await.answer;
        assert_eq!(answer.value, AnswerValue::Interrupted);
    }

    #[tokio::test]
    async fn cancel_all_cancels_pending_questions() {
        let interviewer = Arc::new(ControlInterviewer::new());
        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();

        let ask_interviewer = Arc::clone(&interviewer);
        let ask = tokio::spawn(async move { ask_interviewer.ask(question).await });
        task::yield_now().await;

        interviewer.cancel_all().await;

        let answer = ask.await.unwrap().answer;
        assert_eq!(answer.value, AnswerValue::Cancelled);
    }

    #[tokio::test]
    async fn ask_after_cancel_all_returns_cancelled() {
        let interviewer = ControlInterviewer::new();
        interviewer.cancel_all().await;

        let mut question = Question::new("approve?", QuestionType::YesNo);
        question.id = "q-1".to_string();

        let answer = interviewer.ask(question).await.answer;
        assert_eq!(answer.value, AnswerValue::Cancelled);
    }
}
