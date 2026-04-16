use async_trait::async_trait;

use crate::{Answer, AnswerValue, Interviewer, Question, QuestionType};

/// Always approves: YES for yes/no, first option for multiple choice,
/// "auto-approved" for freeform.
pub struct AutoApproveInterviewer;

#[async_trait]
impl Interviewer for AutoApproveInterviewer {
    async fn ask(&self, question: Question) -> Answer {
        match question.question_type {
            QuestionType::YesNo | QuestionType::Confirmation => Answer::yes(),
            QuestionType::MultipleChoice | QuestionType::MultiSelect => {
                question.options.first().map_or_else(
                    || Answer::text("auto-approved"),
                    |first| Answer {
                        value: AnswerValue::Selected(first.key.clone()),
                        selected_option: Some(first.clone()),
                        text: None,
                    },
                )
            }
            QuestionType::Freeform => Answer::text("auto-approved"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QuestionOption;

    #[tokio::test]
    async fn yes_no_returns_yes() {
        let interviewer = AutoApproveInterviewer;
        let q = Question::new("Approve?", QuestionType::YesNo);
        let answer = interviewer.ask(q).await;
        assert_eq!(answer.value, AnswerValue::Yes);
    }

    #[tokio::test]
    async fn confirmation_returns_yes() {
        let interviewer = AutoApproveInterviewer;
        let q = Question::new("Confirm?", QuestionType::Confirmation);
        let answer = interviewer.ask(q).await;
        assert_eq!(answer.value, AnswerValue::Yes);
    }

    #[tokio::test]
    async fn multiple_choice_returns_first_option() {
        let interviewer = AutoApproveInterviewer;
        let mut q = Question::new("Choose:", QuestionType::MultipleChoice);
        q.options = vec![
            QuestionOption {
                key: "A".to_string(),
                label: "Alpha".to_string(),
            },
            QuestionOption {
                key: "B".to_string(),
                label: "Beta".to_string(),
            },
        ];
        let answer = interviewer.ask(q).await;
        assert_eq!(answer.value, AnswerValue::Selected("A".to_string()));
        assert_eq!(
            answer.selected_option,
            Some(QuestionOption {
                key: "A".to_string(),
                label: "Alpha".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn multiple_choice_no_options_returns_auto_approved() {
        let interviewer = AutoApproveInterviewer;
        let q = Question::new("Choose:", QuestionType::MultipleChoice);
        let answer = interviewer.ask(q).await;
        assert_eq!(answer.value, AnswerValue::Text("auto-approved".to_string()));
    }

    #[tokio::test]
    async fn freeform_returns_auto_approved() {
        let interviewer = AutoApproveInterviewer;
        let q = Question::new("Enter text:", QuestionType::Freeform);
        let answer = interviewer.ask(q).await;
        assert_eq!(answer.value, AnswerValue::Text("auto-approved".to_string()));
        assert_eq!(answer.text, Some("auto-approved".to_string()));
    }
}
