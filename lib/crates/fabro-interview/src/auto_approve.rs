use async_trait::async_trait;
use fabro_types::{Principal, QuestionType, SystemActorKind};

use crate::{Answer, AnswerSubmission, AnswerValue, Interviewer, Question};

/// Always approves: YES for yes/no, first option for multiple choice,
/// "auto-approved" for freeform.
pub struct AutoApproveInterviewer {
    actor: Principal,
}

impl AutoApproveInterviewer {
    #[must_use]
    pub fn new(actor: Principal) -> Self {
        Self { actor }
    }

    #[must_use]
    pub fn engine() -> Self {
        Self::new(Principal::System {
            system_kind: SystemActorKind::Engine,
        })
    }
}

#[async_trait]
impl Interviewer for AutoApproveInterviewer {
    async fn ask(&self, question: Question) -> AnswerSubmission {
        let answer = match question.question_type {
            QuestionType::YesNo | QuestionType::Confirmation => Answer::yes(),
            QuestionType::MultipleChoice | QuestionType::MultiSelect => {
                question.options.first().map_or_else(
                    || Answer::text("auto-approved"),
                    |first| Answer {
                        value:           AnswerValue::Selected(first.key.clone()),
                        selected_option: Some(first.clone()),
                        text:            None,
                    },
                )
            }
            QuestionType::Freeform => Answer::text("auto-approved"),
        };
        AnswerSubmission::new(answer, self.actor.clone())
    }
}

#[cfg(test)]
mod tests {
    use fabro_types::InterviewOption;

    use super::*;

    #[tokio::test]
    async fn yes_no_returns_yes() {
        let interviewer = AutoApproveInterviewer::engine();
        let q = Question::new("Approve?", QuestionType::YesNo);
        let answer = interviewer.ask(q).await.answer;
        assert_eq!(answer.value, AnswerValue::Yes);
    }

    #[tokio::test]
    async fn confirmation_returns_yes() {
        let interviewer = AutoApproveInterviewer::engine();
        let q = Question::new("Confirm?", QuestionType::Confirmation);
        let answer = interviewer.ask(q).await.answer;
        assert_eq!(answer.value, AnswerValue::Yes);
    }

    #[tokio::test]
    async fn multiple_choice_returns_first_option() {
        let interviewer = AutoApproveInterviewer::engine();
        let mut q = Question::new("Choose:", QuestionType::MultipleChoice);
        q.options = vec![
            InterviewOption {
                key:   "A".to_string(),
                label: "Alpha".to_string(),
            },
            InterviewOption {
                key:   "B".to_string(),
                label: "Beta".to_string(),
            },
        ];
        let answer = interviewer.ask(q).await.answer;
        assert_eq!(answer.value, AnswerValue::Selected("A".to_string()));
        assert_eq!(
            answer.selected_option,
            Some(InterviewOption {
                key:   "A".to_string(),
                label: "Alpha".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn multiple_choice_no_options_returns_auto_approved() {
        let interviewer = AutoApproveInterviewer::engine();
        let q = Question::new("Choose:", QuestionType::MultipleChoice);
        let answer = interviewer.ask(q).await.answer;
        assert_eq!(answer.value, AnswerValue::Text("auto-approved".to_string()));
    }

    #[tokio::test]
    async fn freeform_returns_auto_approved() {
        let interviewer = AutoApproveInterviewer::engine();
        let q = Question::new("Enter text:", QuestionType::Freeform);
        let answer = interviewer.ask(q).await.answer;
        assert_eq!(answer.value, AnswerValue::Text("auto-approved".to_string()));
        assert_eq!(answer.text, Some("auto-approved".to_string()));
    }
}
