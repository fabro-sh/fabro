use fabro_llm::types::{ContentPart, Message, Role};

use crate::types::Turn;

#[derive(Debug, Clone, Default)]
pub struct History {
    turns: Vec<Turn>,
}

impl History {
    pub fn push(&mut self, turn: Turn) {
        self.turns.push(turn);
    }

    #[must_use]
    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    pub fn compact(&mut self, preserve_count: usize, summary: String) {
        if self.turns.len() <= preserve_count {
            return;
        }
        let preserved = self.turns.split_off(self.turns.len() - preserve_count);
        let discarded = std::mem::take(&mut self.turns);
        let extracted_user_messages =
            extract_recent_user_messages(discarded, COMPACTION_USER_MESSAGE_TOKEN_BUDGET);
        self.turns.push(Turn::System {
            content:   summary,
            timestamp: std::time::SystemTime::now(),
        });
        self.turns.extend(extracted_user_messages);
        self.turns.extend(preserved);
        self.strip_opaque_provider_items();
    }

    /// Remove provider-specific opaque items that are no longer valid after
    /// compaction. OpenAI reasoning and message items are opaque round-trip
    /// data tied to specific API responses; after compaction replaces their
    /// surrounding context with a summary, they serve no purpose and can
    /// violate API constraints (reasoning must be followed by its
    /// output, identified by the message item's `id`).
    fn strip_opaque_provider_items(&mut self) {
        for turn in &mut self.turns {
            if let Turn::Assistant { provider_parts, .. } = turn {
                provider_parts.retain(|p| !p.is_opaque_openai());
            }
        }
    }

    #[must_use]
    pub fn convert_to_messages(&self) -> Vec<Message> {
        self.turns
            .iter()
            .map(|turn| match turn {
                Turn::User { content, .. } => Message::user(content),
                Turn::Assistant {
                    content,
                    tool_calls,
                    provider_parts,
                    ..
                } => {
                    let mut parts: Vec<ContentPart> = Vec::new();
                    // Provider-specific opaque parts (e.g. OpenAI reasoning items,
                    // Anthropic thinking blocks with signatures) must precede
                    // function calls for correct round-tripping.
                    parts.extend(provider_parts.iter().cloned());
                    if !content.is_empty() {
                        parts.push(ContentPart::text(content));
                    }
                    for tc in tool_calls {
                        parts.push(ContentPart::ToolCall(tc.clone()));
                    }
                    Message {
                        role:         Role::Assistant,
                        content:      parts,
                        name:         None,
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
                    role:         Role::User,
                    content:      vec![ContentPart::text(content)],
                    name:         None,
                    tool_call_id: None,
                },
            })
            .collect()
    }
}

/// Maximum token budget for user messages extracted from discarded turns during
/// compaction.
const COMPACTION_USER_MESSAGE_TOKEN_BUDGET: usize = 20_000;

/// Walk discarded turns in reverse, collecting `Turn::User` variants up to
/// a token budget (estimated at ~4 chars per token). Returns them in
/// chronological order so they can be inserted between the summary and the
/// preserved tail.
fn extract_recent_user_messages(discarded: Vec<Turn>, token_budget: usize) -> Vec<Turn> {
    let char_budget = token_budget * 4;
    let mut total_chars = 0;
    let mut first_kept_index = discarded.len();

    // Walk backward to find the earliest user message within budget
    for (i, turn) in discarded.iter().enumerate().rev() {
        if let Turn::User { content, .. } = turn {
            if total_chars + content.len() > char_budget {
                break;
            }
            total_chars += content.len();
            first_kept_index = i;
        }
    }

    // Collect kept user messages in forward (chronological) order
    discarded
        .into_iter()
        .skip(first_kept_index)
        .filter(|t| matches!(t, Turn::User { .. }))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use fabro_llm::types::{ThinkingData, TokenCounts, ToolCall, ToolResult};

    use super::*;

    #[test]
    fn compact_replaces_old_turns_with_summary() {
        let mut history = History::default();
        for i in 0..8 {
            history.push(Turn::User {
                content:   format!("msg {i}"),
                timestamp: SystemTime::now(),
            });
        }
        history.compact(4, "Summary of old conversation".into());
        // 1 summary + 4 extracted user messages + 4 preserved = 9
        assert_eq!(history.turns().len(), 9);
    }

    #[test]
    fn compact_noop_when_fewer_turns_than_preserve() {
        let mut history = History::default();
        for i in 0..3 {
            history.push(Turn::User {
                content:   format!("msg {i}"),
                timestamp: SystemTime::now(),
            });
        }
        history.compact(6, "Summary".into());
        assert_eq!(history.turns().len(), 3);
    }

    #[test]
    fn compact_preserves_recent_turns() {
        let mut history = History::default();
        for i in 0..8 {
            history.push(Turn::User {
                content:   format!("msg {i}"),
                timestamp: SystemTime::now(),
            });
        }
        history.compact(4, "Summary".into());
        let turns = history.turns();
        // Layout: summary, extracted user msgs (0..3), preserved (4..7)
        assert!(matches!(&turns[0], Turn::System { .. }));
        assert!(matches!(&turns[1], Turn::User { content, .. } if content == "msg 0"));
        assert!(matches!(&turns[2], Turn::User { content, .. } if content == "msg 1"));
        assert!(matches!(&turns[3], Turn::User { content, .. } if content == "msg 2"));
        assert!(matches!(&turns[4], Turn::User { content, .. } if content == "msg 3"));
        assert!(matches!(&turns[5], Turn::User { content, .. } if content == "msg 4"));
        assert!(matches!(&turns[6], Turn::User { content, .. } if content == "msg 5"));
        assert!(matches!(&turns[7], Turn::User { content, .. } if content == "msg 6"));
        assert!(matches!(&turns[8], Turn::User { content, .. } if content == "msg 7"));
    }

    #[test]
    fn compact_summary_maps_to_system_message() {
        let mut history = History::default();
        for i in 0..6 {
            history.push(Turn::User {
                content:   format!("msg {i}"),
                timestamp: SystemTime::now(),
            });
        }
        history.compact(2, "[Context Summary]\nThis is a summary".into());
        let messages = history.convert_to_messages();
        assert_eq!(messages[0].role, Role::System);
        assert!(messages[0].text().contains("[Context Summary]"));
    }

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
            content:   "Hello".into(),
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
            content:        "Hi there".into(),
            tool_calls:     vec![],
            provider_parts: vec![],
            usage:          Box::new(TokenCounts::default()),
            response_id:    "resp_1".into(),
            timestamp:      SystemTime::now(),
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
            content:        "Let me read that".into(),
            tool_calls:     vec![tc],
            provider_parts: vec![],
            usage:          Box::new(TokenCounts::default()),
            response_id:    "resp_2".into(),
            timestamp:      SystemTime::now(),
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
    fn assistant_turn_with_reasoning_in_provider_parts() {
        let mut history = History::default();
        let thinking = ContentPart::Thinking(ThinkingData {
            text:      "Let me think about this...".into(),
            signature: None,
            redacted:  false,
        });
        history.push(Turn::Assistant {
            content:        "The answer is 42".into(),
            tool_calls:     vec![],
            provider_parts: vec![thinking],
            usage:          Box::new(TokenCounts::default()),
            response_id:    "resp_3".into(),
            timestamp:      SystemTime::now(),
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
    fn thinking_with_signature_preserved_via_provider_parts() {
        let mut history = History::default();
        let thinking = ContentPart::Thinking(ThinkingData {
            text:      "Let me think...".into(),
            signature: Some("sig_abc123".into()),
            redacted:  false,
        });
        history.push(Turn::Assistant {
            content:        "The answer".into(),
            tool_calls:     vec![],
            provider_parts: vec![thinking],
            usage:          Box::new(TokenCounts::default()),
            response_id:    "resp_4".into(),
            timestamp:      SystemTime::now(),
        });
        let messages = history.convert_to_messages();
        let thinking_parts: Vec<_> = messages[0]
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Thinking(td) => Some(td),
                _ => None,
            })
            .collect();
        // Should have exactly one thinking block (from provider_parts, not duplicated)
        assert_eq!(thinking_parts.len(), 1);
        // Signature must be preserved
        assert_eq!(thinking_parts[0].signature.as_deref(), Some("sig_abc123"));
    }

    #[test]
    fn assistant_turn_preserves_provider_parts() {
        let mut history = History::default();
        let reasoning_item = ContentPart::Other {
            kind: ContentPart::OPENAI_REASONING.to_string(),
            data: serde_json::json!({"type": "reasoning", "id": "rs_abc"}),
        };
        let tc = ToolCall::new("call_1", "search", serde_json::json!({}));
        history.push(Turn::Assistant {
            content:        String::new(),
            tool_calls:     vec![tc],
            provider_parts: vec![reasoning_item],
            usage:          Box::new(TokenCounts::default()),
            response_id:    "resp_1".into(),
            timestamp:      SystemTime::now(),
        });
        let messages = history.convert_to_messages();
        assert_eq!(messages.len(), 1);
        // Provider parts come first, then tool calls
        assert!(
            matches!(&messages[0].content[0], ContentPart::Other { kind, .. } if kind == ContentPart::OPENAI_REASONING)
        );
        assert!(matches!(&messages[0].content[1], ContentPart::ToolCall(_)));
    }

    #[test]
    fn tool_results_turn_maps_to_tool_message() {
        let mut history = History::default();
        let result = ToolResult::success("call_1", serde_json::json!("file contents here"));
        history.push(Turn::ToolResults {
            results:   vec![result],
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
            content:   "You are a coding assistant".into(),
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
            content:   "Focus on the main task".into(),
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
            content:   "First".into(),
            timestamp: SystemTime::now(),
        });
        assert_eq!(history.turns().len(), 1);
        history.push(Turn::Assistant {
            content:        "Second".into(),
            tool_calls:     vec![],
            provider_parts: vec![],
            usage:          Box::new(TokenCounts::default()),
            response_id:    "resp_1".into(),
            timestamp:      SystemTime::now(),
        });
        assert_eq!(history.turns().len(), 2);
    }

    #[test]
    fn round_trip_preserves_content() {
        let mut history = History::default();
        history.push(Turn::User {
            content:   "Hello".into(),
            timestamp: SystemTime::now(),
        });
        history.push(Turn::Assistant {
            content:        "Hi".into(),
            tool_calls:     vec![ToolCall::new(
                "c1",
                "shell",
                serde_json::json!({"cmd": "ls"}),
            )],
            provider_parts: vec![ContentPart::Thinking(ThinkingData {
                text:      "thinking...".into(),
                signature: None,
                redacted:  false,
            })],
            usage:          Box::new(TokenCounts {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            }),
            response_id:    "resp_1".into(),
            timestamp:      SystemTime::now(),
        });
        history.push(Turn::ToolResults {
            results:   vec![ToolResult::success(
                "c1",
                serde_json::json!("file1.rs\nfile2.rs"),
            )],
            timestamp: SystemTime::now(),
        });

        let messages = history.convert_to_messages();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[2].role, Role::Tool);
    }

    #[test]
    fn compact_strips_openai_reasoning_from_preserved_turns() {
        let mut history = History::default();
        history.push(Turn::User {
            content:   "old msg".into(),
            timestamp: SystemTime::now(),
        });
        history.push(Turn::User {
            content:   "recent msg".into(),
            timestamp: SystemTime::now(),
        });
        let reasoning = ContentPart::Other {
            kind: ContentPart::OPENAI_REASONING.into(),
            data: serde_json::json!({"type": "reasoning", "id": "rs_abc"}),
        };
        let tc = ToolCall::new("call_1", "search", serde_json::json!({}));
        history.push(Turn::Assistant {
            content:        "response".into(),
            tool_calls:     vec![tc],
            provider_parts: vec![reasoning],
            usage:          Box::new(TokenCounts::default()),
            response_id:    "resp_1".into(),
            timestamp:      SystemTime::now(),
        });

        history.compact(2, "Summary".into());

        // Layout: summary, extracted User("old msg"), preserved User("recent msg"),
        // preserved Assistant
        let assistant_turn = &history.turns()[3];
        if let Turn::Assistant {
            provider_parts,
            tool_calls,
            content,
            ..
        } = assistant_turn
        {
            assert!(
                provider_parts.is_empty(),
                "reasoning items should be stripped"
            );
            assert_eq!(tool_calls.len(), 1, "tool_calls should be preserved");
            assert_eq!(content, "response", "text content should be preserved");
        } else {
            panic!("expected Assistant turn");
        }
    }

    #[test]
    fn compact_preserves_anthropic_thinking_blocks() {
        let mut history = History::default();
        history.push(Turn::User {
            content:   "old msg".into(),
            timestamp: SystemTime::now(),
        });
        history.push(Turn::User {
            content:   "recent msg".into(),
            timestamp: SystemTime::now(),
        });
        let thinking = ContentPart::Thinking(ThinkingData {
            text:      "deep thought".into(),
            signature: Some("sig_xyz".into()),
            redacted:  false,
        });
        history.push(Turn::Assistant {
            content:        "answer".into(),
            tool_calls:     vec![],
            provider_parts: vec![thinking],
            usage:          Box::new(TokenCounts::default()),
            response_id:    "resp_1".into(),
            timestamp:      SystemTime::now(),
        });

        history.compact(2, "Summary".into());

        // Layout: summary, extracted User("old msg"), preserved User("recent msg"),
        // preserved Assistant
        let assistant_turn = &history.turns()[3];
        if let Turn::Assistant { provider_parts, .. } = assistant_turn {
            assert_eq!(
                provider_parts.len(),
                1,
                "thinking block should be preserved"
            );
            assert!(matches!(&provider_parts[0], ContentPart::Thinking(_)));
        } else {
            panic!("expected Assistant turn");
        }
    }

    #[test]
    fn compact_strips_reasoning_from_all_preserved_assistant_turns() {
        let mut history = History::default();
        history.push(Turn::User {
            content:   "old msg".into(),
            timestamp: SystemTime::now(),
        });
        // Two assistant turns that will both be preserved
        for i in 0..2 {
            history.push(Turn::Assistant {
                content:        format!("response {i}"),
                tool_calls:     vec![],
                provider_parts: vec![ContentPart::Other {
                    kind: ContentPart::OPENAI_REASONING.into(),
                    data: serde_json::json!({"type": "reasoning", "id": format!("rs_{i}")}),
                }],
                usage:          Box::new(TokenCounts::default()),
                response_id:    format!("resp_{i}"),
                timestamp:      SystemTime::now(),
            });
        }

        history.compact(2, "Summary".into());

        for turn in history.turns() {
            if let Turn::Assistant { provider_parts, .. } = turn {
                assert!(
                    provider_parts.is_empty(),
                    "all reasoning items should be stripped from all assistant turns"
                );
            }
        }
    }

    #[test]
    fn extract_recent_user_messages_collects_in_chronological_order() {
        let turns = vec![
            Turn::User {
                content:   "first".into(),
                timestamp: SystemTime::now(),
            },
            Turn::Assistant {
                content:        "reply".into(),
                tool_calls:     vec![],
                provider_parts: vec![],
                usage:          Box::new(TokenCounts::default()),
                response_id:    "r1".into(),
                timestamp:      SystemTime::now(),
            },
            Turn::User {
                content:   "second".into(),
                timestamp: SystemTime::now(),
            },
        ];
        let extracted = extract_recent_user_messages(turns, 20_000);
        assert_eq!(extracted.len(), 2);
        assert!(matches!(&extracted[0], Turn::User { content, .. } if content == "first"));
        assert!(matches!(&extracted[1], Turn::User { content, .. } if content == "second"));
    }

    #[test]
    fn extract_recent_user_messages_respects_token_budget() {
        let turns = vec![
            Turn::User {
                content:   "a".repeat(100),
                timestamp: SystemTime::now(),
            },
            Turn::User {
                content:   "b".repeat(100),
                timestamp: SystemTime::now(),
            },
        ];
        // Budget of 30 tokens = 120 chars; second message (100 chars) fits, first would
        // exceed
        let extracted = extract_recent_user_messages(turns, 30);
        assert_eq!(extracted.len(), 1);
        assert!(matches!(&extracted[0], Turn::User { content, .. } if content.starts_with('b')));
    }

    #[test]
    fn compact_extracts_only_user_turns_from_discarded() {
        let mut history = History::default();
        history.push(Turn::User {
            content:   "user msg".into(),
            timestamp: SystemTime::now(),
        });
        history.push(Turn::Assistant {
            content:        "assistant msg".into(),
            tool_calls:     vec![],
            provider_parts: vec![],
            usage:          Box::new(TokenCounts::default()),
            response_id:    "r1".into(),
            timestamp:      SystemTime::now(),
        });
        history.push(Turn::User {
            content:   "preserved".into(),
            timestamp: SystemTime::now(),
        });

        history.compact(1, "Summary".into());

        // Layout: summary, extracted User("user msg"), preserved User("preserved")
        assert_eq!(history.turns().len(), 3);
        assert!(matches!(&history.turns()[0], Turn::System { .. }));
        assert!(matches!(&history.turns()[1], Turn::User { content, .. } if content == "user msg"));
        assert!(
            matches!(&history.turns()[2], Turn::User { content, .. } if content == "preserved")
        );
    }
}
