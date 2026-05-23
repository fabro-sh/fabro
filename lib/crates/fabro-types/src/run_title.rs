use std::error::Error;
use std::fmt;

use fabro_util::text::strip_goal_decoration;

pub const MAX_RUN_TITLE_CHARS: usize = 100;
const TRUNCATED_RUN_TITLE_CHARS: usize = MAX_RUN_TITLE_CHARS - 3;
const UNTITLED_RUN: &str = "Untitled run";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunTitleError {
    Blank,
    TooLong { max_chars: usize },
    NotSingleLine,
}

impl fmt::Display for RunTitleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blank => write!(f, "run title must not be blank"),
            Self::TooLong { max_chars } => {
                write!(f, "run title must be at most {max_chars} characters")
            }
            Self::NotSingleLine => write!(f, "run title must be a single line"),
        }
    }
}

impl Error for RunTitleError {}

#[must_use]
pub fn infer_run_title(goal: &str) -> String {
    let stripped = strip_goal_decoration(goal).trim();
    if stripped.is_empty() {
        return UNTITLED_RUN.to_string();
    }

    let char_count = stripped.chars().count();
    if char_count <= MAX_RUN_TITLE_CHARS {
        return stripped.to_string();
    }

    let truncated: String = stripped.chars().take(TRUNCATED_RUN_TITLE_CHARS).collect();
    format!("{truncated}...")
}

pub fn normalize_explicit_run_title(title: &str) -> Result<String, RunTitleError> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err(RunTitleError::Blank);
    }
    if trimmed.chars().any(char::is_control) {
        return Err(RunTitleError::NotSingleLine);
    }
    if trimmed.chars().count() > MAX_RUN_TITLE_CHARS {
        return Err(RunTitleError::TooLong {
            max_chars: MAX_RUN_TITLE_CHARS,
        });
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::{RunTitleError, infer_run_title, normalize_explicit_run_title};

    #[test]
    fn explicit_title_normalization_trims_valid_titles() {
        assert_eq!(
            normalize_explicit_run_title("  Ship dashboard polish  ").unwrap(),
            "Ship dashboard polish"
        );
    }

    #[test]
    fn explicit_title_normalization_rejects_blank_titles() {
        assert_eq!(
            normalize_explicit_run_title(" \t ").unwrap_err(),
            RunTitleError::Blank
        );
    }

    #[test]
    fn explicit_title_normalization_rejects_control_characters() {
        assert_eq!(
            normalize_explicit_run_title("First\nSecond").unwrap_err(),
            RunTitleError::NotSingleLine
        );
        assert_eq!(
            normalize_explicit_run_title("First\tSecond").unwrap_err(),
            RunTitleError::NotSingleLine
        );
    }

    #[test]
    fn explicit_title_normalization_rejects_titles_over_100_chars() {
        let title = "x".repeat(101);
        assert_eq!(
            normalize_explicit_run_title(&title).unwrap_err(),
            RunTitleError::TooLong { max_chars: 100 }
        );
    }

    #[test]
    fn inferred_title_strips_goal_decoration() {
        assert_eq!(infer_run_title("## Plan: migrate DB"), "migrate DB");
    }

    #[test]
    fn inferred_title_truncates_to_100_chars_with_ellipsis() {
        let title = infer_run_title(&"a".repeat(101));
        assert_eq!(title.chars().count(), 100);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn inferred_title_falls_back_for_blank_goals() {
        assert_eq!(infer_run_title(" \nmore detail"), "Untitled run");
    }
}
