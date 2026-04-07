use std::fmt;

use serde::{Deserialize, Serialize};

use crate::run_event::InterviewOption;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InterviewQuestionType {
    YesNo,
    MultipleChoice,
    MultiSelect,
    #[default]
    Freeform,
    Confirmation,
}

impl InterviewQuestionType {
    #[must_use]
    pub fn from_wire_name(value: &str) -> Self {
        match value {
            "yes_no" => Self::YesNo,
            "multiple_choice" => Self::MultipleChoice,
            "multi_select" => Self::MultiSelect,
            "freeform" => Self::Freeform,
            "confirmation" => Self::Confirmation,
            _ => Self::Freeform,
        }
    }
}

impl fmt::Display for InterviewQuestionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::YesNo => write!(f, "yes_no"),
            Self::MultipleChoice => write!(f, "multiple_choice"),
            Self::MultiSelect => write!(f, "multi_select"),
            Self::Freeform => write!(f, "freeform"),
            Self::Confirmation => write!(f, "confirmation"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct InterviewQuestionRecord {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub question_type: InterviewQuestionType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<InterviewOption>,
    #[serde(default)]
    pub allow_freeform: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_display: Option<String>,
}
