use std::io::IsTerminal;

use arc_util::terminal::Styles;
use async_trait::async_trait;
use dialoguer::console::Term;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::{Answer, AnswerValue, Interviewer, Question, QuestionType};

/// Reads from stdin to collect answers. Displays formatted prompts per spec 6.4.
pub struct ConsoleInterviewer {
    styles: &'static Styles,
}

impl ConsoleInterviewer {
    #[must_use]
    pub const fn new(styles: &'static Styles) -> Self {
        Self { styles }
    }
}

fn find_matching_option(response: &str, options: &[super::QuestionOption]) -> Option<Answer> {
    let trimmed = response.trim();
    // Try matching by key (case-insensitive)
    for opt in options {
        if opt.key.eq_ignore_ascii_case(trimmed) {
            return Some(Answer {
                value: AnswerValue::Selected(opt.key.clone()),
                selected_option: Some(opt.clone()),
                text: None,
            });
        }
    }
    // Try matching by 1-based index
    if let Ok(idx) = trimmed.parse::<usize>() {
        if idx >= 1 && idx <= options.len() {
            let opt = &options[idx - 1];
            return Some(Answer {
                value: AnswerValue::Selected(opt.key.clone()),
                selected_option: Some(opt.clone()),
                text: None,
            });
        }
    }
    None
}

async fn read_line(prompt: &str) -> std::io::Result<String> {
    // Print the prompt to stderr so it doesn't interfere with piped stdout
    eprint!("{prompt}");
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    Ok(line.trim_end().to_string())
}

/// Ask a multiple-choice question using dialoguer's `Select` widget on a TTY.
fn ask_select_interactive(question: &Question) -> Answer {
    let items: Vec<String> = question
        .options
        .iter()
        .map(|opt| format!("{} - {}", opt.key, opt.label))
        .collect();

    let has_freeform = question.allow_freeform;
    let mut all_items = items;
    if has_freeform {
        all_items.push("Other (free text)...".to_string());
    }

    let selection = dialoguer::Select::new()
        .with_prompt(&question.text)
        .items(&all_items)
        .default(0)
        .interact_on_opt(&Term::stderr());

    match selection {
        Ok(Some(idx)) if has_freeform && idx == question.options.len() => {
            // User chose the free-text option
            dialoguer::Input::<String>::new()
                .with_prompt("Enter your response")
                .interact_on(&Term::stderr())
                .map_or_else(|_| Answer::skipped(), Answer::text)
        }
        Ok(Some(idx)) if idx < question.options.len() => {
            let opt = &question.options[idx];
            Answer {
                value: AnswerValue::Selected(opt.key.clone()),
                selected_option: Some(opt.clone()),
                text: None,
            }
        }
        _ => Answer::skipped(),
    }
}

/// Ask a multi-select question using dialoguer's `MultiSelect` widget on a TTY.
fn ask_multi_select_interactive(question: &Question) -> Answer {
    let items: Vec<String> = question
        .options
        .iter()
        .map(|opt| format!("{} - {}", opt.key, opt.label))
        .collect();

    let selection = dialoguer::MultiSelect::new()
        .with_prompt(&question.text)
        .items(&items)
        .interact_on_opt(&Term::stderr());

    match selection {
        Ok(Some(indices)) if !indices.is_empty() => {
            let idx = indices[0];
            let opt = &question.options[idx];
            Answer {
                value: AnswerValue::Selected(opt.key.clone()),
                selected_option: Some(opt.clone()),
                text: None,
            }
        }
        _ => Answer::skipped(),
    }
}

/// Ask a yes/no or confirmation question using dialoguer's `Confirm` widget on a TTY.
fn ask_confirm_interactive(question: &Question) -> Answer {
    let confirmed = dialoguer::Confirm::new()
        .with_prompt(&question.text)
        .default(true)
        .interact_on_opt(&Term::stderr());

    match confirmed {
        Ok(Some(true)) => Answer::yes(),
        _ => Answer::no(),
    }
}

/// Ask a freeform question using dialoguer's `Input` widget on a TTY.
fn ask_freeform_interactive(question: &Question) -> Answer {
    dialoguer::Input::<String>::new()
        .with_prompt(&question.text)
        .interact_on(&Term::stderr())
        .map_or_else(|_| Answer::skipped(), Answer::text)
}

#[async_trait]
impl Interviewer for ConsoleInterviewer {
    async fn ask(&self, question: Question) -> Answer {
        // If stdin is a TTY, use dialoguer for interactive arrow-key navigation.
        // Otherwise, fall back to the line-based reader for piped input.
        if std::io::stdin().is_terminal() {
            let q = question;
            return tokio::task::spawn_blocking(move || match q.question_type {
                QuestionType::MultipleChoice => ask_select_interactive(&q),
                QuestionType::MultiSelect => ask_multi_select_interactive(&q),
                QuestionType::YesNo | QuestionType::Confirmation => ask_confirm_interactive(&q),
                QuestionType::Freeform => ask_freeform_interactive(&q),
            })
            .await
            .unwrap_or_else(|_| Answer::skipped());
        }

        // Non-TTY fallback: line-based stdin reading
        let s = self.styles;
        eprintln!(
            "{bold}{cyan}?{reset} {}",
            question.text,
            bold = s.bold,
            cyan = s.cyan,
            reset = s.reset,
        );

        match question.question_type {
            QuestionType::MultipleChoice | QuestionType::MultiSelect => {
                for (i, opt) in question.options.iter().enumerate() {
                    eprintln!(
                        "  {dim}[{reset}{bold}{}{reset}{dim}]{reset} {} - {}",
                        i + 1,
                        opt.key,
                        opt.label,
                        dim = s.dim,
                        bold = s.bold,
                        reset = s.reset,
                    );
                }
                if question.allow_freeform {
                    eprintln!("  Or type a free-text response");
                }
                let response = read_line("Select: ").await.unwrap_or_default();
                if let Some(answer) = find_matching_option(&response, &question.options) {
                    return answer;
                }
                if question.allow_freeform {
                    return Answer::text(response);
                }
                // Fallback: try match again (spec says to do this)
                find_matching_option(&response, &question.options).unwrap_or_else(Answer::skipped)
            }
            QuestionType::YesNo | QuestionType::Confirmation => {
                let response = read_line("[Y/N]: ").await.unwrap_or_default();
                let trimmed = response.trim().to_lowercase();
                if trimmed == "y" || trimmed == "yes" {
                    Answer::yes()
                } else {
                    Answer::no()
                }
            }
            QuestionType::Freeform => {
                let response = read_line("> ").await.unwrap_or_default();
                Answer::text(response)
            }
        }
    }

    async fn inform(&self, message: &str, stage: &str) {
        let s = self.styles;
        eprintln!(
            "{dim}[{stage}]{reset} {message}",
            dim = s.dim,
            reset = s.reset,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_matching_option_by_key() {
        let options = vec![
            super::super::QuestionOption {
                key: "A".to_string(),
                label: "Approve".to_string(),
            },
            super::super::QuestionOption {
                key: "R".to_string(),
                label: "Reject".to_string(),
            },
        ];
        let result = find_matching_option("A", &options);
        assert!(result.is_some());
        let answer = result.unwrap();
        assert_eq!(answer.value, AnswerValue::Selected("A".to_string()));
    }

    #[test]
    fn find_matching_option_by_key_case_insensitive() {
        let options = vec![super::super::QuestionOption {
            key: "Y".to_string(),
            label: "Yes".to_string(),
        }];
        let result = find_matching_option("y", &options);
        assert!(result.is_some());
    }

    #[test]
    fn find_matching_option_by_index() {
        let options = vec![
            super::super::QuestionOption {
                key: "A".to_string(),
                label: "Alpha".to_string(),
            },
            super::super::QuestionOption {
                key: "B".to_string(),
                label: "Beta".to_string(),
            },
        ];
        let result = find_matching_option("2", &options);
        assert!(result.is_some());
        let answer = result.unwrap();
        assert_eq!(answer.value, AnswerValue::Selected("B".to_string()));
    }

    #[test]
    fn find_matching_option_no_match() {
        let options = vec![super::super::QuestionOption {
            key: "A".to_string(),
            label: "Alpha".to_string(),
        }];
        let result = find_matching_option("zzz", &options);
        assert!(result.is_none());
    }

    #[test]
    fn find_matching_option_index_out_of_range() {
        let options = vec![super::super::QuestionOption {
            key: "A".to_string(),
            label: "Alpha".to_string(),
        }];
        let result = find_matching_option("5", &options);
        assert!(result.is_none());
    }
}
