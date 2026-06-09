//! Pure mapping between canonical types and the Chat Completions wire shapes.

use super::wire::{ChatFunction, ChatMessage, ChatToolCall};
use crate::types::{
    ContentPart, FinishReason, Message, Request, ResponseFormat, ResponseFormatType, Role,
    ToolChoice, ToolDefinition,
};

pub(super) fn map_finish_reason(reason: Option<&str>) -> FinishReason {
    match reason {
        Some("stop") | None => FinishReason::Stop,
        Some("length") => FinishReason::Length,
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("content_filter") => FinishReason::ContentFilter,
        Some(other) => FinishReason::Other(other.to_string()),
    }
}

/// Build the content string from a message's parts, including fallback text
/// for unsupported content types (Audio, Document).
fn content_text_with_fallbacks(parts: &[ContentPart]) -> String {
    let mut segments: Vec<String> = Vec::new();
    for part in parts {
        match part {
            ContentPart::Text(text) => segments.push(text.clone()),
            ContentPart::Audio(_) => {
                segments.push("[Audio content not supported by this provider]".to_string());
            }
            ContentPart::Document(doc) => {
                let desc = doc.file_name.as_ref().map_or_else(
                    || "[Document content not supported by this provider]".to_string(),
                    |name| {
                        format!("[Document '{name}': content type not supported by this provider]")
                    },
                );
                segments.push(desc);
            }
            _ => {}
        }
    }
    segments.join("")
}

pub(super) fn translate_messages(messages: &[Message]) -> Vec<ChatMessage> {
    messages
        .iter()
        .flat_map(|msg| {
            // Tool messages must be split into one ChatMessage per ToolResult,
            // each with its own tool_call_id. The Chat Completions API requires
            // every tool_call_id from the assistant to have a matching tool message.
            if msg.role == Role::Tool {
                return msg
                    .content
                    .iter()
                    .filter_map(|part| {
                        if let ContentPart::ToolResult(tr) = part {
                            let output = tr
                                .content
                                .as_str()
                                .map_or_else(|| tr.content.to_string(), str::to_string);
                            Some(ChatMessage {
                                role:              "tool".to_string(),
                                content:           Some(output),
                                reasoning_content: None,
                                tool_call_id:      Some(tr.tool_call_id.clone()),
                                tool_calls:        None,
                            })
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
            }

            let role = match msg.role {
                Role::System | Role::Developer => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => unreachable!(
                    "Role::Tool is handled in the early-return branch above this match"
                ),
            };

            let mut tool_calls: Vec<ChatToolCall> = Vec::new();
            if msg.role == Role::Assistant {
                for part in &msg.content {
                    if let ContentPart::ToolCall(tc) = part {
                        let arguments = tc
                            .raw_arguments
                            .clone()
                            .unwrap_or_else(|| tc.arguments.to_string());
                        tool_calls.push(ChatToolCall {
                            id:       tc.id.clone(),
                            kind:     "function".to_string(),
                            function: ChatFunction {
                                name: tc.name.clone(),
                                arguments,
                            },
                        });
                    }
                }
            }

            let text = content_text_with_fallbacks(&msg.content);
            let content = if text.is_empty() { None } else { Some(text) };
            let tool_calls = if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            };

            // Extract reasoning/thinking content for assistant messages.
            let reasoning_content = if msg.role == Role::Assistant {
                let reasoning: String = msg
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Thinking(t) if !t.redacted => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if reasoning.is_empty() {
                    None
                } else {
                    Some(reasoning)
                }
            } else {
                None
            };

            vec![ChatMessage {
                role: role.to_string(),
                content,
                reasoning_content,
                tool_call_id: msg.tool_call_id.clone(),
                tool_calls,
            }]
        })
        .collect()
}

pub(super) fn translate_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })
        })
        .collect()
}

pub(super) fn translate_tool_choice(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => serde_json::json!("auto"),
        ToolChoice::None => serde_json::json!("none"),
        ToolChoice::Required => serde_json::json!("required"),
        ToolChoice::Named { tool_name } => {
            serde_json::json!({"type": "function", "function": {"name": tool_name}})
        }
    }
}

pub(super) fn custom_tool_names(request: &Request) -> Vec<String> {
    request
        .tools
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|tool| tool.is_custom())
        .map(|tool| tool.name.clone())
        .collect()
}

pub(super) fn parse_tool_arguments(
    tool_name: &str,
    raw_arguments: &str,
    custom_tool_names: &[String],
) -> serde_json::Value {
    match serde_json::from_str(raw_arguments) {
        Ok(arguments) => arguments,
        Err(_) if custom_tool_names.iter().any(|name| name == tool_name) => {
            serde_json::Value::String(raw_arguments.to_string())
        }
        Err(_) => serde_json::json!({}),
    }
}

/// Translate unified `ResponseFormat` to Chat Completions `response_format`.
pub(super) fn translate_response_format(format: &ResponseFormat) -> serde_json::Value {
    match format.kind {
        ResponseFormatType::Text => serde_json::json!({"type": "text"}),
        ResponseFormatType::JsonObject => serde_json::json!({"type": "json_object"}),
        ResponseFormatType::JsonSchema => {
            let mut json_schema = serde_json::json!({
                "name": "response",
                "strict": format.strict,
            });
            if let Some(schema) = &format.json_schema {
                json_schema["schema"] = schema.clone();
            }
            serde_json::json!({
                "type": "json_schema",
                "json_schema": json_schema,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AudioData, ContentPart, DocumentData, Message, Role, ToolCall};

    #[test]
    fn translate_assistant_message_with_tool_calls_only() {
        let msg = Message {
            role:         Role::Assistant,
            content:      vec![ContentPart::ToolCall(ToolCall::new(
                "call_1",
                "get_weather",
                serde_json::json!({"city": "SF"}),
            ))],
            name:         None,
            tool_call_id: None,
        };
        let translated = translate_messages(&[msg]);
        assert_eq!(translated.len(), 1);
        assert_eq!(translated[0].role, "assistant");
        assert!(translated[0].content.is_none());
        let tool_calls = translated[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].kind, "function");
        assert_eq!(tool_calls[0].function.name, "get_weather");
        assert_eq!(tool_calls[0].function.arguments, r#"{"city":"SF"}"#);
    }

    #[test]
    fn translate_assistant_message_with_text_and_tool_calls() {
        let msg = Message {
            role:         Role::Assistant,
            content:      vec![
                ContentPart::text("Let me check the weather"),
                ContentPart::ToolCall(ToolCall::new(
                    "call_2",
                    "get_weather",
                    serde_json::json!({"city": "NYC"}),
                )),
            ],
            name:         None,
            tool_call_id: None,
        };
        let translated = translate_messages(&[msg]);
        assert_eq!(
            translated[0].content.as_deref(),
            Some("Let me check the weather")
        );
        let tool_calls = translated[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.name, "get_weather");
    }

    #[test]
    fn translate_assistant_message_with_raw_arguments() {
        let mut tc = ToolCall::new("call_3", "search", serde_json::json!({"q": "rust"}));
        tc.raw_arguments = Some(r#"{"q": "rust"}"#.to_string());
        let msg = Message {
            role:         Role::Assistant,
            content:      vec![ContentPart::ToolCall(tc)],
            name:         None,
            tool_call_id: None,
        };
        let translated = translate_messages(&[msg]);
        let tool_calls = translated[0].tool_calls.as_ref().unwrap();
        // Should prefer raw_arguments over serializing arguments
        assert_eq!(tool_calls[0].function.arguments, r#"{"q": "rust"}"#);
    }

    #[test]
    fn translate_tool_message_has_tool_call_id() {
        let msg = Message::tool_result(
            "call_1",
            serde_json::Value::String("72F and sunny".into()),
            false,
        );
        let translated = translate_messages(&[msg]);
        assert_eq!(translated[0].role, "tool");
        assert_eq!(translated[0].tool_call_id.as_deref(), Some("call_1"));
        assert!(translated[0].tool_calls.is_none());
    }

    #[test]
    fn translate_user_message_has_no_tool_calls() {
        let msg = Message::user("Hello");
        let translated = translate_messages(&[msg]);
        assert_eq!(translated[0].role, "user");
        assert_eq!(translated[0].content.as_deref(), Some("Hello"));
        assert!(translated[0].tool_calls.is_none());
    }

    #[test]
    fn assistant_tool_calls_serialize_correctly() {
        let msg = Message {
            role:         Role::Assistant,
            content:      vec![ContentPart::ToolCall(ToolCall::new(
                "call_1",
                "get_weather",
                serde_json::json!({"city": "SF"}),
            ))],
            name:         None,
            tool_call_id: None,
        };
        let translated = translate_messages(&[msg]);
        let json = serde_json::to_value(&translated[0]).unwrap();
        assert!(json.get("content").is_none());
        assert!(json.get("tool_call_id").is_none());
        let tool_calls = json["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(tool_calls[0]["id"], "call_1");
        assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
    }

    #[test]
    fn audio_content_produces_text_fallback() {
        let msg = Message {
            role:         Role::User,
            content:      vec![ContentPart::Audio(AudioData {
                url:        Some("https://example.com/audio.wav".to_string()),
                data:       None,
                media_type: None,
            })],
            name:         None,
            tool_call_id: None,
        };
        let translated = translate_messages(&[msg]);
        assert_eq!(
            translated[0].content.as_deref(),
            Some("[Audio content not supported by this provider]")
        );
    }

    #[test]
    fn document_content_produces_text_fallback_with_filename() {
        let msg = Message {
            role:         Role::User,
            content:      vec![ContentPart::Document(DocumentData {
                url:        Some("https://example.com/doc.pdf".to_string()),
                data:       None,
                media_type: None,
                file_name:  Some("report.pdf".to_string()),
            })],
            name:         None,
            tool_call_id: None,
        };
        let translated = translate_messages(&[msg]);
        assert_eq!(
            translated[0].content.as_deref(),
            Some("[Document 'report.pdf': content type not supported by this provider]")
        );
    }

    #[test]
    fn document_content_produces_text_fallback_without_filename() {
        let msg = Message {
            role:         Role::User,
            content:      vec![ContentPart::Document(DocumentData {
                url:        None,
                data:       Some(vec![1, 2, 3]),
                media_type: None,
                file_name:  None,
            })],
            name:         None,
            tool_call_id: None,
        };
        let translated = translate_messages(&[msg]);
        assert_eq!(
            translated[0].content.as_deref(),
            Some("[Document content not supported by this provider]")
        );
    }

    #[test]
    fn mixed_text_and_audio_content_concatenates() {
        let msg = Message {
            role:         Role::User,
            content:      vec![
                ContentPart::text("Check this: "),
                ContentPart::Audio(AudioData {
                    url:        None,
                    data:       Some(vec![1, 2]),
                    media_type: None,
                }),
            ],
            name:         None,
            tool_call_id: None,
        };
        let translated = translate_messages(&[msg]);
        assert_eq!(
            translated[0].content.as_deref(),
            Some("Check this: [Audio content not supported by this provider]")
        );
    }
}
