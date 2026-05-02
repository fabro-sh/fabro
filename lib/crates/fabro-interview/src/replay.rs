use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use fabro_types::SystemActorKind;

use crate::{Answer, AnswerSubmission, Interviewer, Question};

/// Replays recorded answers in sequence. When recordings are exhausted,
/// returns `Answer::interrupted()`.
pub struct ReplayInterviewer {
    submissions: Mutex<VecDeque<AnswerSubmission>>,
}

impl ReplayInterviewer {
    /// Creates a new `ReplayInterviewer` from recorded question-answer
    /// submissions.
    #[must_use]
    pub fn new(recordings: Vec<(Question, AnswerSubmission)>) -> Self {
        let submissions = recordings
            .into_iter()
            .map(|(_, submission)| submission)
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
        submissions.pop_front().unwrap_or_else(|| {
            AnswerSubmission::system(Answer::interrupted(), SystemActorKind::Engine)
        })
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::{AuthMethod, IdpIdentity, Principal, QuestionType};

    use super::*;
    use crate::AnswerValue;

    #[tokio::test]
    async fn replays_recorded_answers() {
        let actor = Principal::user(
            IdpIdentity::new("https://github.com", "12345").unwrap(),
            "octocat".to_string(),
            AuthMethod::Github,
        );
        let recordings = vec![
            (
                Question::new("approve?", QuestionType::YesNo),
                AnswerSubmission::new(Answer::yes(), actor.clone()),
            ),
            (
                Question::new("name?", QuestionType::Freeform),
                AnswerSubmission::new(Answer::text("Alice"), actor.clone()),
            ),
        ];

        let replayer = ReplayInterviewer::new(recordings);

        let s1 = replayer
            .ask(Question::new("anything", QuestionType::YesNo))
            .await;
        assert_eq!(s1.answer.value, AnswerValue::Yes);
        assert_eq!(s1.actor, actor);

        let s2 = replayer
            .ask(Question::new("anything", QuestionType::Freeform))
            .await;
        assert_eq!(s2.answer.value, AnswerValue::Text("Alice".to_string()));
        assert_eq!(s2.actor, actor);
    }

    #[tokio::test]
    async fn returns_interrupted_when_exhausted() {
        let recordings = vec![(
            Question::new("approve?", QuestionType::YesNo),
            AnswerSubmission::system(Answer::yes(), SystemActorKind::Engine),
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
