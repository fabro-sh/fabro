use crate::types::Turn;
use unified_llm::types::{ContentPart, Message, Role};

#[derive(Debug, Clone, Default)]
pub struct History {
    turns: Vec<Turn>,
}

impl History {
    pub fn push(&mut self, turn: Turn) {
        self.turns.push(turn);
    }

    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    pub fn convert_to_messages(&self) -> Vec<Message> {
        self.turns
            .iter()
            .map(|turn| match turn {
                Turn::User { content, .. } => Message::user(content),
                Turn::Assistant {
                    content,
                    tool_calls,
                    reasoning,
                    ..
                } => {
                    let mut parts: Vec<ContentPart> = Vec::new();
                    if let Some(reasoning_text) = reasoning {
                        parts.push(ContentPart::Thinking(
                            unified_llm::types::ThinkingData {
                                text: reasoning_text.clone(),
                                signature: None,
                                redacted: false,
                            },
                        ));
                    }
                    if !content.is_empty() {
                        parts.push(ContentPart::text(content));
                    }
                    for tc in tool_calls {
                        parts.push(ContentPart::ToolCall(tc.clone()));
                    }
                    Message {
                        role: Role::Assistant,
                        content: parts,
                        name: None,
                        tool_call_id: None,
                    }
                }
                Turn::ToolResults { results, .. } => {
                    let content: Vec<ContentPart> = results
                        .iter()
                        .map(|r| ContentPart::ToolResult(r.clone()))
                        .collect();
                    // Use the first result's tool_call_id if available
                    let tool_call_id = results.first().map(|r| r.tool_call_id.clone());
                    Message {
                        role: Role::Tool,
                        content,
                        name: None,
                        tool_call_id,
                    }
                }
                Turn::System { content, .. } => Message::system(content),
                Turn::Steering { content, .. } => Message {
                    role: Role::User,
                    content: vec![ContentPart::text(content)],
                    name: None,
                    tool_call_id: None,
                },
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    use unified_llm::types::{ToolCall, ToolResult, Usage};

    #[test]
    fn empty_history_produces_empty_messages() {
        let history = History::default();
        assert!(history.convert_to_messages().is_empty());
        assert_eq!(history.turns().len(), 0);
    }

    #[test]
    fn user_turn_maps_to_user_message() {
        let mut history = History::default();
        history.push(Turn::User {
            content: "Hello".into(),
            timestamp: SystemTime::now(),
        });
        let messages = history.convert_to_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].text(), "Hello");
    }

    #[test]
    fn assistant_turn_maps_to_assistant_message() {
        let mut history = History::default();
        history.push(Turn::Assistant {
            content: "Hi there".into(),
            tool_calls: vec![],
            reasoning: None,
            usage: Usage::default(),
            response_id: "resp_1".into(),
            timestamp: SystemTime::now(),
        });
        let messages = history.convert_to_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[0].text(), "Hi there");
    }

    #[test]
    fn assistant_turn_with_tool_calls() {
        let mut history = History::default();
        let tc = ToolCall::new("call_1", "read_file", serde_json::json!({"path": "foo.rs"}));
        history.push(Turn::Assistant {
            content: "Let me read that".into(),
            tool_calls: vec![tc],
            reasoning: None,
            usage: Usage::default(),
            response_id: "resp_2".into(),
            timestamp: SystemTime::now(),
        });
        let messages = history.convert_to_messages();
        assert_eq!(messages[0].role, Role::Assistant);
        let tool_call_parts: Vec<_> = messages[0]
            .content
            .iter()
            .filter(|p| matches!(p, ContentPart::ToolCall(_)))
            .collect();
        assert_eq!(tool_call_parts.len(), 1);
    }

    #[test]
    fn assistant_turn_with_reasoning() {
        let mut history = History::default();
        history.push(Turn::Assistant {
            content: "The answer is 42".into(),
            tool_calls: vec![],
            reasoning: Some("Let me think about this...".into()),
            usage: Usage::default(),
            response_id: "resp_3".into(),
            timestamp: SystemTime::now(),
        });
        let messages = history.convert_to_messages();
        let thinking_parts: Vec<_> = messages[0]
            .content
            .iter()
            .filter(|p| matches!(p, ContentPart::Thinking(_)))
            .collect();
        assert_eq!(thinking_parts.len(), 1);
    }

    #[test]
    fn tool_results_turn_maps_to_tool_message() {
        let mut history = History::default();
        let result = ToolResult {
            tool_call_id: "call_1".into(),
            content: serde_json::json!("file contents here"),
            is_error: false,
            image_data: None,
            image_media_type: None,
        };
        history.push(Turn::ToolResults {
            results: vec![result],
            timestamp: SystemTime::now(),
        });
        let messages = history.convert_to_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::Tool);
        assert_eq!(messages[0].tool_call_id, Some("call_1".into()));
    }

    #[test]
    fn system_turn_maps_to_system_message() {
        let mut history = History::default();
        history.push(Turn::System {
            content: "You are a coding assistant".into(),
            timestamp: SystemTime::now(),
        });
        let messages = history.convert_to_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[0].text(), "You are a coding assistant");
    }

    #[test]
    fn steering_turn_maps_to_user_message() {
        let mut history = History::default();
        history.push(Turn::Steering {
            content: "Focus on the main task".into(),
            timestamp: SystemTime::now(),
        });
        let messages = history.convert_to_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].text(), "Focus on the main task");
    }

    #[test]
    fn turns_len_matches_push_count() {
        let mut history = History::default();
        assert_eq!(history.turns().len(), 0);
        history.push(Turn::User {
            content: "First".into(),
            timestamp: SystemTime::now(),
        });
        assert_eq!(history.turns().len(), 1);
        history.push(Turn::Assistant {
            content: "Second".into(),
            tool_calls: vec![],
            reasoning: None,
            usage: Usage::default(),
            response_id: "resp_1".into(),
            timestamp: SystemTime::now(),
        });
        assert_eq!(history.turns().len(), 2);
    }

    #[test]
    fn round_trip_preserves_content() {
        let mut history = History::default();
        history.push(Turn::User {
            content: "Hello".into(),
            timestamp: SystemTime::now(),
        });
        history.push(Turn::Assistant {
            content: "Hi".into(),
            tool_calls: vec![ToolCall::new(
                "c1",
                "shell",
                serde_json::json!({"cmd": "ls"}),
            )],
            reasoning: Some("thinking...".into()),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
                ..Default::default()
            },
            response_id: "resp_1".into(),
            timestamp: SystemTime::now(),
        });
        history.push(Turn::ToolResults {
            results: vec![ToolResult {
                tool_call_id: "c1".into(),
                content: serde_json::json!("file1.rs\nfile2.rs"),
                is_error: false,
                image_data: None,
                image_media_type: None,
            }],
            timestamp: SystemTime::now(),
        });

        let messages = history.convert_to_messages();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[2].role, Role::Tool);
    }
}
