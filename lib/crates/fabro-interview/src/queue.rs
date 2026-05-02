use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use fabro_types::{Principal, SystemActorKind};

use crate::{Answer, AnswerSubmission, Interviewer, Question};

/// Reads answers from a pre-filled queue. Returns Interrupted when empty.
pub struct QueueInterviewer {
    answers: Mutex<VecDeque<Answer>>,
    actor:   Principal,
}

impl QueueInterviewer {
    #[must_use]
    pub fn new(answers: VecDeque<Answer>) -> Self {
        Self::with_actor(answers, Principal::system(SystemActorKind::Engine))
    }

    #[must_use]
    pub fn with_actor(answers: VecDeque<Answer>, actor: Principal) -> Self {
        Self {
            answers: Mutex::new(answers),
            actor,
        }
    }
}

#[async_trait]
impl Interviewer for QueueInterviewer {
    async fn ask(&self, _question: Question) -> AnswerSubmission {
        let mut queue = self.answers.lock().expect("queue lock poisoned");
        AnswerSubmission::new(
            queue.pop_front().unwrap_or_else(Answer::interrupted),
            self.actor.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::QuestionType;

    use super::*;
    use crate::AnswerValue;

    #[tokio::test]
    async fn returns_queued_answers_in_order() {
        let answers = VecDeque::from([Answer::yes(), Answer::no()]);
        let interviewer = QueueInterviewer::new(answers);
        let q = Question::new("q1", QuestionType::YesNo);

        let a1 = interviewer.ask(q.clone()).await.answer;
        assert_eq!(a1.value, AnswerValue::Yes);

        let a2 = interviewer.ask(q).await.answer;
        assert_eq!(a2.value, AnswerValue::No);
    }

    #[tokio::test]
    async fn returns_interrupted_when_empty() {
        let interviewer = QueueInterviewer::new(VecDeque::new());
        let q = Question::new("q", QuestionType::YesNo);
        let answer = interviewer.ask(q).await.answer;
        assert_eq!(answer.value, AnswerValue::Interrupted);
    }

    #[tokio::test]
    async fn returns_interrupted_after_exhausted() {
        let answers = VecDeque::from([Answer::yes()]);
        let interviewer = QueueInterviewer::new(answers);
        let q = Question::new("q", QuestionType::YesNo);

        let _ = interviewer.ask(q.clone()).await;
        let answer = interviewer.ask(q).await.answer;
        assert_eq!(answer.value, AnswerValue::Interrupted);
    }
}
