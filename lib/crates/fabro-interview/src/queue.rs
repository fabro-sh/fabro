use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::{Answer, Interviewer, Question};

/// Reads answers from a pre-filled queue. Returns Aborted when empty.
pub struct QueueInterviewer {
    answers: Mutex<VecDeque<Answer>>,
}

impl QueueInterviewer {
    #[must_use]
    pub const fn new(answers: VecDeque<Answer>) -> Self {
        Self {
            answers: Mutex::new(answers),
        }
    }
}

#[async_trait]
impl Interviewer for QueueInterviewer {
    async fn ask(&self, _question: Question) -> Answer {
        let mut queue = self.answers.lock().expect("queue lock poisoned");
        queue.pop_front().unwrap_or_else(Answer::aborted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnswerValue, QuestionType};

    #[tokio::test]
    async fn returns_queued_answers_in_order() {
        let answers = VecDeque::from([Answer::yes(), Answer::no()]);
        let interviewer = QueueInterviewer::new(answers);
        let q = Question::new("q1", QuestionType::YesNo);

        let a1 = interviewer.ask(q.clone()).await;
        assert_eq!(a1.value, AnswerValue::Yes);

        let a2 = interviewer.ask(q).await;
        assert_eq!(a2.value, AnswerValue::No);
    }

    #[tokio::test]
    async fn returns_aborted_when_empty() {
        let interviewer = QueueInterviewer::new(VecDeque::new());
        let q = Question::new("q", QuestionType::YesNo);
        let answer = interviewer.ask(q).await;
        assert_eq!(answer.value, AnswerValue::Aborted);
    }

    #[tokio::test]
    async fn returns_aborted_after_exhausted() {
        let answers = VecDeque::from([Answer::yes()]);
        let interviewer = QueueInterviewer::new(answers);
        let q = Question::new("q", QuestionType::YesNo);

        let _ = interviewer.ask(q.clone()).await;
        let answer = interviewer.ask(q).await;
        assert_eq!(answer.value, AnswerValue::Aborted);
    }
}
