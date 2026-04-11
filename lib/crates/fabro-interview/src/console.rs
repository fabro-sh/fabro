use std::io::IsTerminal;

use async_trait::async_trait;
use dialoguer::console::Term;
use dialoguer::theme::ColorfulTheme;
use fabro_util::terminal::Styles;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::task;

use crate::{Answer, AnswerValue, Interviewer, Question, QuestionOption, QuestionType};

enum PromptRead {
    Line(String),
    Eof,
    Error,
}

/// Reads from stdin to collect answers. Displays formatted prompts per spec
/// 6.4.
pub struct ConsoleInterviewer {
    styles: &'static Styles,
}

impl ConsoleInterviewer {
    #[must_use]
    pub fn new(styles: &'static Styles) -> Self {
        Self { styles }
    }
}

fn find_matching_option(response: &str, options: &[QuestionOption]) -> Option<Answer> {
    let trimmed = response.trim();
    // Try matching by key (case-insensitive)
    for opt in options {
        if opt.key.eq_ignore_ascii_case(trimmed) {
            return Some(Answer {
                value:           AnswerValue::Selected(opt.key.clone()),
                selected_option: Some(opt.clone()),
                text:            None,
            });
        }
    }
    // Try matching by 1-based index
    if let Ok(idx) = trimmed.parse::<usize>() {
        if idx >= 1 && idx <= options.len() {
            let opt = &options[idx - 1];
            return Some(Answer {
                value:           AnswerValue::Selected(opt.key.clone()),
                selected_option: Some(opt.clone()),
                text:            None,
            });
        }
    }
    None
}

#[allow(clippy::print_stderr)]
async fn read_line(prompt: &str) -> PromptRead {
    // Print the prompt to stderr so it doesn't interfere with piped stdout
    eprint!("{prompt}");
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();
    match reader.read_line(&mut line).await {
        Ok(0) => PromptRead::Eof,
        Ok(_) => PromptRead::Line(line.trim_end().to_string()),
        Err(_) => PromptRead::Error,
    }
}

fn parse_non_tty_choice_response(question: &Question, prompt_read: PromptRead) -> Answer {
    let PromptRead::Line(response) = prompt_read else {
        return Answer::interrupted();
    };
    if response.trim().is_empty() {
        return Answer::interrupted();
    }
    if let Some(answer) = find_matching_option(&response, &question.options) {
        return answer;
    }
    if question.allow_freeform {
        return Answer::text(response);
    }
    find_matching_option(&response, &question.options).unwrap_or_else(Answer::interrupted)
}

fn parse_non_tty_confirm_response(prompt_read: PromptRead) -> Answer {
    let PromptRead::Line(response) = prompt_read else {
        return Answer::interrupted();
    };
    match response.trim().to_lowercase().as_str() {
        "y" | "yes" => Answer::yes(),
        "n" | "no" => Answer::no(),
        _ => Answer::interrupted(),
    }
}

fn parse_non_tty_freeform_response(prompt_read: PromptRead) -> Answer {
    let PromptRead::Line(response) = prompt_read else {
        return Answer::interrupted();
    };
    if response.trim().is_empty() {
        Answer::interrupted()
    } else {
        Answer::text(response)
    }
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

    let selection = dialoguer::Select::with_theme(&ColorfulTheme::default())
        .with_prompt(&question.text)
        .items(&all_items)
        .default(0)
        .interact_on_opt(&Term::stderr());

    match selection {
        Ok(Some(idx)) if has_freeform && idx == question.options.len() => {
            // User chose the free-text option
            dialoguer::Input::<String>::with_theme(&ColorfulTheme::default())
                .with_prompt("Enter your response")
                .interact_on(&Term::stderr())
                .map_or_else(
                    |_| Answer::interrupted(),
                    |response| {
                        if response.trim().is_empty() {
                            Answer::interrupted()
                        } else {
                            Answer::text(response)
                        }
                    },
                )
        }
        Ok(Some(idx)) if idx < question.options.len() => {
            let opt = &question.options[idx];
            Answer {
                value:           AnswerValue::Selected(opt.key.clone()),
                selected_option: Some(opt.clone()),
                text:            None,
            }
        }
        _ => Answer::interrupted(),
    }
}

/// Ask a multi-select question using dialoguer's `MultiSelect` widget on a TTY.
fn ask_multi_select_interactive(question: &Question) -> Answer {
    let items: Vec<String> = question
        .options
        .iter()
        .map(|opt| format!("{} - {}", opt.key, opt.label))
        .collect();

    let selection = dialoguer::MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt(&question.text)
        .items(&items)
        .interact_on_opt(&Term::stderr());

    match selection {
        Ok(Some(indices)) if !indices.is_empty() => {
            let keys: Vec<String> = indices
                .iter()
                .map(|&i| question.options[i].key.clone())
                .collect();
            Answer::multi_selected(keys)
        }
        _ => Answer::interrupted(),
    }
}

/// Ask a yes/no or confirmation question using dialoguer's `Confirm` widget on
/// a TTY.
fn ask_confirm_interactive(question: &Question) -> Answer {
    let confirmed = dialoguer::Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(&question.text)
        .default(true)
        .interact_on_opt(&Term::stderr());

    match confirmed {
        Ok(Some(true)) => Answer::yes(),
        Ok(Some(false)) => Answer::no(),
        _ => Answer::interrupted(),
    }
}

/// Ask a freeform question using dialoguer's `Input` widget on a TTY.
fn ask_freeform_interactive(question: &Question) -> Answer {
    dialoguer::Input::<String>::with_theme(&ColorfulTheme::default())
        .with_prompt(&question.text)
        .interact_on(&Term::stderr())
        .map_or_else(
            |_| Answer::interrupted(),
            |response| {
                if response.trim().is_empty() {
                    Answer::interrupted()
                } else {
                    Answer::text(response)
                }
            },
        )
}

#[async_trait]
impl Interviewer for ConsoleInterviewer {
    #[allow(clippy::print_stderr)]
    async fn ask(&self, question: Question) -> Answer {
        // If stdin is a TTY, use dialoguer for interactive arrow-key navigation.
        // Otherwise, fall back to the line-based reader for piped input.
        if std::io::stdin().is_terminal() {
            if let Some(ref context_text) = question.context_display {
                let rendered = self.styles.render_markdown(context_text);
                eprint!("{rendered}");
            }
            let q = question;
            return task::spawn_blocking(move || match q.question_type {
                QuestionType::MultipleChoice => ask_select_interactive(&q),
                QuestionType::MultiSelect => ask_multi_select_interactive(&q),
                QuestionType::YesNo | QuestionType::Confirmation => ask_confirm_interactive(&q),
                QuestionType::Freeform => ask_freeform_interactive(&q),
            })
            .await
            .unwrap_or_else(|_| Answer::interrupted());
        }

        // Non-TTY fallback: line-based stdin reading
        let s = self.styles;
        eprintln!("{} {}", s.bold_cyan.apply_to("?"), question.text,);

        match question.question_type {
            QuestionType::MultipleChoice | QuestionType::MultiSelect => {
                for (i, opt) in question.options.iter().enumerate() {
                    eprintln!(
                        "  {}{}{}  {} - {}",
                        s.dim.apply_to("["),
                        s.bold.apply_to(i + 1),
                        s.dim.apply_to("]"),
                        opt.key,
                        opt.label,
                    );
                }
                if question.allow_freeform {
                    eprintln!("  Or type a free-text response");
                }
                parse_non_tty_choice_response(&question, read_line("Select: ").await)
            }
            QuestionType::YesNo | QuestionType::Confirmation => {
                parse_non_tty_confirm_response(read_line("[Y/N]: ").await)
            }
            QuestionType::Freeform => parse_non_tty_freeform_response(read_line("> ").await),
        }
    }

    #[allow(clippy::print_stderr)]
    async fn inform(&self, message: &str, stage: &str) {
        let s = self.styles;
        eprintln!("{} {message}", s.dim.apply_to(format!("[{stage}]")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_matching_option_by_key() {
        let options = vec![
            crate::QuestionOption {
                key:   "A".to_string(),
                label: "Approve".to_string(),
            },
            crate::QuestionOption {
                key:   "R".to_string(),
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
        let options = vec![crate::QuestionOption {
            key:   "Y".to_string(),
            label: "Yes".to_string(),
        }];
        let result = find_matching_option("y", &options);
        assert!(result.is_some());
    }

    #[test]
    fn find_matching_option_by_index() {
        let options = vec![
            crate::QuestionOption {
                key:   "A".to_string(),
                label: "Alpha".to_string(),
            },
            crate::QuestionOption {
                key:   "B".to_string(),
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
        let options = vec![crate::QuestionOption {
            key:   "A".to_string(),
            label: "Alpha".to_string(),
        }];
        let result = find_matching_option("zzz", &options);
        assert!(result.is_none());
    }

    #[test]
    fn find_matching_option_index_out_of_range() {
        let options = vec![crate::QuestionOption {
            key:   "A".to_string(),
            label: "Alpha".to_string(),
        }];
        let result = find_matching_option("5", &options);
        assert!(result.is_none());
    }

    #[test]
    fn non_tty_multiple_choice_eof_returns_interrupted() {
        let mut question = Question::new("Approve?", QuestionType::MultipleChoice);
        question.options = vec![crate::QuestionOption {
            key:   "A".to_string(),
            label: "Approve".to_string(),
        }];

        let answer = parse_non_tty_choice_response(&question, PromptRead::Eof);
        assert_eq!(answer.value, AnswerValue::Interrupted);
    }

    #[test]
    fn non_tty_confirmation_invalid_response_returns_interrupted() {
        let answer = parse_non_tty_confirm_response(PromptRead::Line(String::new()));
        assert_eq!(answer.value, AnswerValue::Interrupted);
    }

    #[test]
    fn non_tty_freeform_blank_response_returns_interrupted() {
        let answer = parse_non_tty_freeform_response(PromptRead::Line("   ".to_string()));
        assert_eq!(answer.value, AnswerValue::Interrupted);
    }
}
