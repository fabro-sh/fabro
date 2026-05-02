use async_trait::async_trait;
use fabro_types::{Principal, SystemActorKind};

use crate::{Answer, AnswerSubmission, Interviewer, Question};

/// Delegates question answering to a provided callback function.
pub struct CallbackInterviewer {
    callback: Box<dyn Fn(Question) -> Answer + Send + Sync>,
    actor:    Principal,
}

impl CallbackInterviewer {
    pub fn new(callback: impl Fn(Question) -> Answer + Send + Sync + 'static) -> Self {
        Self::with_actor(
            Principal::System {
                system_kind: SystemActorKind::Engine,
            },
            callback,
        )
    }

    pub fn with_actor(
        actor: Principal,
        callback: impl Fn(Question) -> Answer + Send + Sync + 'static,
    ) -> Self {
        Self {
            callback: Box::new(callback),
            actor,
        }
    }
}

#[async_trait]
impl Interviewer for CallbackInterviewer {
    async fn ask(&self, question: Question) -> AnswerSubmission {
        AnswerSubmission::new((self.callback)(question), self.actor.clone())
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::QuestionType;

    use super::*;
    use crate::AnswerValue;

    #[tokio::test]
    async fn calls_callback_with_question() {
        let interviewer = CallbackInterviewer::new(|q| {
            if q.question_type == QuestionType::YesNo {
                Answer::yes()
            } else {
                Answer::no()
            }
        });

        let yes_q = Question::new("approve?", QuestionType::YesNo);
        let answer = interviewer.ask(yes_q).await.answer;
        assert_eq!(answer.value, AnswerValue::Yes);

        let no_q = Question::new("choose:", QuestionType::MultipleChoice);
        let answer = interviewer.ask(no_q).await.answer;
        assert_eq!(answer.value, AnswerValue::No);
    }

    #[tokio::test]
    async fn callback_receives_question_text() {
        let interviewer = CallbackInterviewer::new(|q| Answer::text(q.text));
        let q = Question::new("hello world", QuestionType::Freeform);
        let answer = interviewer.ask(q).await.answer;
        assert_eq!(answer.text, Some("hello world".to_string()));
    }
}
