use serde::{Deserialize, Serialize};

use crate::run_event::InterviewOption;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    Default,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum InterviewQuestionType {
    YesNo,
    MultipleChoice,
    MultiSelect,
    #[default]
    Freeform,
    Confirmation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct InterviewQuestionRecord {
    #[serde(default)]
    pub id:              String,
    #[serde(default)]
    pub text:            String,
    #[serde(default)]
    pub stage:           String,
    #[serde(default)]
    pub question_type:   InterviewQuestionType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options:         Vec<InterviewOption>,
    #[serde(default)]
    pub allow_freeform:  bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_display: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_type_wire_names_roundtrip() {
        let cases = [
            ("yes_no", InterviewQuestionType::YesNo),
            ("multiple_choice", InterviewQuestionType::MultipleChoice),
            ("multi_select", InterviewQuestionType::MultiSelect),
            ("freeform", InterviewQuestionType::Freeform),
            ("confirmation", InterviewQuestionType::Confirmation),
        ];

        for (wire, question_type) in cases {
            assert_eq!(
                wire.parse::<InterviewQuestionType>().unwrap(),
                question_type
            );
            assert_eq!(question_type.to_string(), wire);
        }
    }
}
