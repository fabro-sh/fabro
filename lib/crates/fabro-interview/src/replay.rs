use std::sync::Mutex;

use async_trait::async_trait;

use crate::{Answer, Interviewer, Question};

/// Replays recorded answers in sequence. When recordings are exhausted,
/// returns `Answer::aborted()`.
pub struct ReplayInterviewer {
    answers: Mutex<Vec<Answer>>,
}

impl ReplayInterviewer {
    /// Creates a new `ReplayInterviewer` from a list of recorded question-answer pairs.
    /// Only the answers are retained for replay.
    #[must_use]
    pub fn new(recordings: Vec<(Question, Answer)>) -> Self {
        let answers: Vec<Answer> = recordings.into_iter().map(|(_, a)| a).collect();
        Self {
            answers: Mutex::new(answers),
        }
    }
}

#[async_trait]
impl Interviewer for ReplayInterviewer {
    async fn ask(&self, _question: Question) -> Answer {
        let mut answers = self.answers.lock().expect("answers lock poisoned");
        if answers.is_empty() {
            Answer::aborted()
        } else {
            answers.remove(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnswerValue, QuestionType};

    #[tokio::test]
    async fn replays_recorded_answers() {
        let recordings = vec![
            (
                Question::new("approve?", QuestionType::YesNo),
                Answer::yes(),
            ),
            (
                Question::new("name?", QuestionType::Freeform),
                Answer::text("Alice"),
            ),
        ];

        let replayer = ReplayInterviewer::new(recordings);

        let a1 = replayer
            .ask(Question::new("anything", QuestionType::YesNo))
            .await;
        assert_eq!(a1.value, AnswerValue::Yes);

        let a2 = replayer
            .ask(Question::new("anything", QuestionType::Freeform))
            .await;
        assert_eq!(a2.value, AnswerValue::Text("Alice".to_string()));
    }

    #[tokio::test]
    async fn returns_aborted_when_exhausted() {
        let recordings = vec![(
            Question::new("approve?", QuestionType::YesNo),
            Answer::yes(),
        )];

        let replayer = ReplayInterviewer::new(recordings);

        let a1 = replayer
            .ask(Question::new("first", QuestionType::YesNo))
            .await;
        assert_eq!(a1.value, AnswerValue::Yes);

        let a2 = replayer
            .ask(Question::new("second", QuestionType::YesNo))
            .await;
        assert_eq!(a2.value, AnswerValue::Aborted);

        let a3 = replayer
            .ask(Question::new("third", QuestionType::YesNo))
            .await;
        assert_eq!(a3.value, AnswerValue::Aborted);
    }
}
