use std::sync::Mutex;

use async_trait::async_trait;
use fabro_types::{Principal, SystemActorKind};

use crate::{Answer, AnswerSubmission, Interviewer, Question};

/// Replays recorded answers in sequence. When recordings are exhausted,
/// returns `Answer::interrupted()`.
pub struct ReplayInterviewer {
    submissions: Mutex<Vec<AnswerSubmission>>,
}

impl ReplayInterviewer {
    /// Creates a new `ReplayInterviewer` from a list of recorded
    /// question-answer pairs. Only the answers are retained for replay.
    #[must_use]
    pub fn new(recordings: Vec<(Question, Answer)>) -> Self {
        let actor = Principal::system(SystemActorKind::Engine);
        let submissions: Vec<AnswerSubmission> = recordings
            .into_iter()
            .map(|(_, answer)| AnswerSubmission::new(answer, actor.clone()))
            .collect();
        Self {
            submissions: Mutex::new(submissions),
        }
    }
}

#[async_trait]
impl Interviewer for ReplayInterviewer {
    async fn ask(&self, _question: Question) -> AnswerSubmission {
        let mut submissions = self.submissions.lock().expect("answers lock poisoned");
        if submissions.is_empty() {
            AnswerSubmission::new(
                Answer::interrupted(),
                Principal::system(SystemActorKind::Engine),
            )
        } else {
            submissions.remove(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::QuestionType;

    use super::*;
    use crate::AnswerValue;

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
            .await
            .answer;
        assert_eq!(a1.value, AnswerValue::Yes);

        let a2 = replayer
            .ask(Question::new("anything", QuestionType::Freeform))
            .await
            .answer;
        assert_eq!(a2.value, AnswerValue::Text("Alice".to_string()));
    }

    #[tokio::test]
    async fn returns_interrupted_when_exhausted() {
        let recordings = vec![(
            Question::new("approve?", QuestionType::YesNo),
            Answer::yes(),
        )];

        let replayer = ReplayInterviewer::new(recordings);

        let a1 = replayer
            .ask(Question::new("first", QuestionType::YesNo))
            .await
            .answer;
        assert_eq!(a1.value, AnswerValue::Yes);

        let a2 = replayer
            .ask(Question::new("second", QuestionType::YesNo))
            .await
            .answer;
        assert_eq!(a2.value, AnswerValue::Interrupted);

        let a3 = replayer
            .ask(Question::new("third", QuestionType::YesNo))
            .await
            .answer;
        assert_eq!(a3.value, AnswerValue::Interrupted);
    }
}
