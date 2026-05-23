use crate::history::History;
use crate::types::Message;

const TASK_REMINDER_TURN_THRESHOLD: usize = 10;

pub(crate) const TASK_REMINDER_TEXT: &str = "\
<system-reminder>
TaskCreate and TaskUpdate are available but have not been used in the last 10 assistant turns. For multi-step work, create tasks with TaskCreate and keep progress current with TaskUpdate.
</system-reminder>";

pub(crate) fn maybe_reminder(history: &History, available_tool_names: &[&str]) -> Option<String> {
    if !task_management_tools_available(available_tool_names) {
        return None;
    }

    let counts = turn_counts(history);
    (counts.assistant_turns_since_task_management >= TASK_REMINDER_TURN_THRESHOLD
        && counts.assistant_turns_since_reminder >= TASK_REMINDER_TURN_THRESHOLD)
        .then(|| TASK_REMINDER_TEXT.to_string())
}

fn task_management_tools_available(tool_names: &[&str]) -> bool {
    tool_names.contains(&"TaskCreate") && tool_names.contains(&"TaskUpdate")
}

#[derive(Debug, Clone, Copy, Default)]
struct TurnCounts {
    assistant_turns_since_task_management: usize,
    assistant_turns_since_reminder:        usize,
}

fn turn_counts(history: &History) -> TurnCounts {
    let mut found_task_management = false;
    let mut found_reminder = false;
    let mut counts = TurnCounts::default();

    for turn in history.turns().iter().rev() {
        match turn {
            Message::Assistant { tool_calls, .. } => {
                if !found_task_management
                    && tool_calls
                        .iter()
                        .any(|call| matches!(call.name.as_str(), "TaskCreate" | "TaskUpdate"))
                {
                    found_task_management = true;
                }

                if !found_task_management {
                    counts.assistant_turns_since_task_management += 1;
                }
                if !found_reminder {
                    counts.assistant_turns_since_reminder += 1;
                }
            }
            Message::System { content, .. } if !found_reminder && is_task_reminder(content) => {
                found_reminder = true;
            }
            _ => {}
        }

        if found_task_management && found_reminder {
            break;
        }
    }

    counts
}

fn is_task_reminder(content: &str) -> bool {
    content.trim() == TASK_REMINDER_TEXT
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use fabro_llm::types::{TokenCounts, ToolCall};

    use super::*;
    fn assistant(tool_name: Option<&str>) -> Message {
        let tool_calls = tool_name
            .map(|name| vec![ToolCall::new("call_1", name, serde_json::json!({}))])
            .unwrap_or_default();
        Message::Assistant {
            content: String::new(),
            tool_calls,
            provider_parts: Vec::new(),
            usage: Box::<TokenCounts>::default(),
            response_id: "resp".into(),
            timestamp: SystemTime::now(),
        }
    }

    fn system(content: &str) -> Message {
        Message::System {
            content:   content.into(),
            timestamp: SystemTime::now(),
        }
    }

    fn history_from(turns: Vec<Message>) -> History {
        let mut history = History::default();
        for turn in turns {
            history.push(turn);
        }
        history
    }

    #[test]
    fn injects_after_ten_assistant_turns_without_task_management() {
        let history = history_from((0..10).map(|_| assistant(None)).collect());

        assert_eq!(
            maybe_reminder(&history, &["TaskCreate", "TaskUpdate"]).as_deref(),
            Some(TASK_REMINDER_TEXT)
        );
    }

    #[test]
    fn respects_ten_assistant_turn_cooldown_after_reminder() {
        let mut turns = vec![system(TASK_REMINDER_TEXT)];
        turns.extend((0..9).map(|_| assistant(None)));
        let history = history_from(turns);
        assert!(maybe_reminder(&history, &["TaskCreate", "TaskUpdate"]).is_none());

        let mut turns = vec![system(TASK_REMINDER_TEXT)];
        turns.extend((0..10).map(|_| assistant(None)));
        let history = history_from(turns);
        assert!(maybe_reminder(&history, &["TaskCreate", "TaskUpdate"]).is_some());
    }

    #[test]
    fn skips_when_task_management_tools_are_unavailable() {
        let history = history_from((0..10).map(|_| assistant(None)).collect());

        assert!(maybe_reminder(&history, &["TaskCreate"]).is_none());
        assert!(maybe_reminder(&history, &["TaskUpdate"]).is_none());
        assert!(maybe_reminder(&history, &["TaskList", "TaskGet"]).is_none());
    }

    #[test]
    fn resets_after_task_create_or_task_update() {
        for tool_name in ["TaskCreate", "TaskUpdate"] {
            let mut turns: Vec<Message> = (0..10).map(|_| assistant(None)).collect();
            turns.push(assistant(Some(tool_name)));
            turns.extend((0..9).map(|_| assistant(None)));
            let history = history_from(turns);
            assert!(
                maybe_reminder(&history, &["TaskCreate", "TaskUpdate"]).is_none(),
                "tool {tool_name} should reset reminder counter"
            );
        }
    }
}
