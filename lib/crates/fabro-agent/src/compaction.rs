use std::fmt::Write;

use fabro_llm::client::Client;
use fabro_llm::types::{Message, Request};
use tracing::debug;

use crate::agent_profile::AgentProfile;
use crate::error::Error;
use crate::event::Emitter;
use crate::file_tracker::FileTracker;
use crate::history::History;
use crate::truncation;
use crate::types::{AgentEvent, Turn};

/// Check whether the context window usage exceeds the configured threshold.
/// Emits a `Warning` event with kind `"context_window"` when over the
/// threshold. Returns `true` if the threshold is exceeded.
pub fn check_context_usage(
    system_prompt: &str,
    history: &History,
    provider_profile: &dyn AgentProfile,
    threshold_percent: usize,
    emitter: &Emitter,
    session_id: &str,
) -> bool {
    let estimated_tokens = estimate_token_count(system_prompt, history);
    let context_window = provider_profile.context_window_size();
    let threshold = context_window * threshold_percent / 100;

    if estimated_tokens > threshold {
        emitter.emit(session_id.to_owned(), AgentEvent::Warning {
            kind:    "context_window".into(),
            message: format!(
                "Context window usage: {}%",
                estimated_tokens * 100 / context_window
            ),
            details: serde_json::json!({
                "estimated_tokens": estimated_tokens,
                "context_window_size": context_window,
                "usage_percent": estimated_tokens * 100 / context_window,
            }),
        });
        true
    } else {
        false
    }
}

/// Compact the conversation history by summarizing older turns via a
/// non-streaming LLM call.
#[allow(
    clippy::too_many_arguments,
    reason = "Context compaction needs explicit history, model, tracking, and emission inputs."
)]
pub async fn compact_context(
    history: &mut History,
    llm_client: &Client,
    provider_profile: &dyn AgentProfile,
    system_prompt: &str,
    file_tracker: &FileTracker,
    preserve_count: usize,
    emitter: &Emitter,
    session_id: &str,
) -> Result<(), Error> {
    let estimated_tokens = estimate_token_count(system_prompt, history);
    let context_window = provider_profile.context_window_size();
    let original_turn_count = history.turns().len();

    emitter.emit(session_id.to_owned(), AgentEvent::CompactionStarted {
        estimated_tokens,
        context_window_size: context_window,
    });

    // Determine turns to summarize
    if original_turn_count <= preserve_count {
        return Ok(());
    }
    let turns_to_summarize = &history.turns()[..original_turn_count - preserve_count];
    let rendered = render_turns_for_summary(turns_to_summarize);

    // Build structured summarization prompt
    let file_ops_section = if file_tracker.is_empty() {
        String::new()
    } else {
        format!(
            "\n## File Operations\nCOPY THIS SECTION VERBATIM into your summary.\n\n{}",
            file_tracker.render()
        )
    };

    let summarization_prompt = format!(
        "You are creating a handoff document for a different coding assistant that will take over \
this task. That assistant will only see your summary and the most recent messages — nothing else \
from the conversation so far.\n\n\
Write a summary using EXACTLY these sections:\n\n\
## Goal\nWhat the user asked for and any constraints or preferences stated.\n\n\
## Progress\nWhat was accomplished, with file paths and key decisions.\n\n\
## Key Decisions\nImportant choices made and their rationale.\n\n\
## Failed Approaches\nWhat was tried and didn't work, and why.\n\n\
## Open Issues\nBugs, edge cases, or TODOs that remain.\n\n\
## Next Steps\nWhat should happen next to make progress.\n\n\
Be thorough and specific — the assistant taking over has no prior context. Include file paths, \
function names, error messages, and exact values. Omit pleasantries and conversational filler.\
{file_ops_section}"
    );

    let summary_request = Request {
        model:            provider_profile.model().to_string(),
        messages:         vec![
            Message::system(summarization_prompt),
            Message::user(format!(
                "Here is the conversation to summarize:\n\n{rendered}"
            )),
        ],
        provider:         Some(provider_profile.provider().to_string()),
        tools:            None,
        tool_choice:      None,
        response_format:  None,
        temperature:      None,
        top_p:            None,
        max_tokens:       Some(4096),
        stop_sequences:   None,
        reasoning_effort: None,
        speed:            None,
        metadata:         None,
        provider_options: None,
    };

    let response = llm_client
        .complete(&summary_request)
        .await
        .map_err(Error::Llm)?;

    let summary_text = response.text();
    debug!(
        summary_len = summary_text.len(),
        "Compaction summary generated"
    );
    let summary_content = format!(
        "A different assistant began this task and produced the following summary. \
Build on their progress — do not repeat completed steps.\n\n{summary_text}"
    );
    let summary_token_estimate = summary_content.len() / 4;

    history.compact(preserve_count, summary_content);

    emitter.emit(session_id.to_owned(), AgentEvent::CompactionCompleted {
        original_turn_count,
        preserved_turn_count: preserve_count,
        summary_token_estimate,
        tracked_file_count: file_tracker.file_count(),
    });

    Ok(())
}

/// Estimate the total token count of the system prompt and conversation
/// history. Uses a rough heuristic of ~4 characters per token.
pub fn estimate_token_count(system_prompt: &str, history: &History) -> usize {
    let mut total_chars = system_prompt.len();

    for turn in history.turns() {
        match turn {
            Turn::User { content, .. } => total_chars += content.len(),
            Turn::Assistant {
                content,
                tool_calls,
                ..
            } => {
                total_chars += content.len();
                if let Some(r) = turn.reasoning_text() {
                    total_chars += r.len();
                }
                for tc in tool_calls {
                    total_chars += tc.name.len();
                    total_chars += tc.arguments.to_string().len();
                }
            }
            Turn::ToolResults { results, .. } => {
                for r in results {
                    total_chars += r.content.to_string().len();
                }
            }
            Turn::System { content, .. } | Turn::Steering { content, .. } => {
                total_chars += content.len();
            }
        }
    }

    total_chars / 4 // rough estimate: ~4 chars per token
}

/// Render conversation turns into a human-readable summary format for the
/// compaction LLM call.
pub fn render_turns_for_summary(turns: &[Turn]) -> String {
    let mut out = String::new();
    for turn in turns {
        match turn {
            Turn::User { content, .. } => {
                let _ = writeln!(out, "User: {content}");
            }
            Turn::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if !content.is_empty() {
                    let _ = writeln!(out, "Assistant: {content}");
                }
                for tc in tool_calls {
                    let args_str = tc.arguments.to_string();
                    let truncated = if args_str.len() > 500 {
                        format!(
                            "{}...",
                            &args_str[..truncation::floor_char_boundary(&args_str, 500)]
                        )
                    } else {
                        args_str
                    };
                    let _ = writeln!(out, "[Tool call: {}] {truncated}", tc.name);
                }
            }
            Turn::ToolResults { results, .. } => {
                for r in results {
                    let content_str = r.content.to_string();
                    let truncated = if content_str.len() > 500 {
                        format!(
                            "{}...",
                            &content_str[..truncation::floor_char_boundary(&content_str, 500)]
                        )
                    } else {
                        content_str
                    };
                    let _ = writeln!(out, "[Tool result: {}] {truncated}", r.tool_call_id);
                }
            }
            Turn::System { content, .. } => {
                let _ = writeln!(out, "System: {content}");
            }
            Turn::Steering { content, .. } => {
                let _ = writeln!(out, "Steering: {content}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use fabro_llm::types::{TokenCounts, ToolCall, ToolResult};

    use super::*;
    use crate::event::Emitter;
    use crate::history::History;
    use crate::test_support::TestProfile;
    use crate::tool_registry::ToolRegistry;
    use crate::types::Turn;

    #[test]
    fn render_turns_produces_labeled_text() {
        let turns = vec![
            Turn::User {
                content:   "Hello".into(),
                timestamp: SystemTime::now(),
            },
            Turn::Assistant {
                content:        "Let me check".into(),
                tool_calls:     vec![ToolCall::new(
                    "c1",
                    "read_file",
                    serde_json::json!({"path": "foo.rs"}),
                )],
                provider_parts: vec![],
                usage:          Box::new(TokenCounts::default()),
                response_id:    "resp_1".into(),
                timestamp:      SystemTime::now(),
            },
            Turn::ToolResults {
                results:   vec![ToolResult {
                    tool_call_id:     "c1".into(),
                    content:          serde_json::json!("file contents here"),
                    is_error:         false,
                    image_data:       None,
                    image_media_type: None,
                }],
                timestamp: SystemTime::now(),
            },
        ];
        let rendered = render_turns_for_summary(&turns);
        assert!(rendered.contains("User:"));
        assert!(rendered.contains("Hello"));
        assert!(rendered.contains("Assistant:"));
        assert!(rendered.contains("Let me check"));
        assert!(rendered.contains("[Tool call: read_file]"));
        assert!(rendered.contains("[Tool result: c1]"));
    }

    #[test]
    fn render_turns_truncates_long_tool_output() {
        let long_output = "x".repeat(1000);
        let turns = vec![Turn::ToolResults {
            results:   vec![ToolResult {
                tool_call_id:     "c1".into(),
                content:          serde_json::json!(long_output),
                is_error:         false,
                image_data:       None,
                image_media_type: None,
            }],
            timestamp: SystemTime::now(),
        }];
        let rendered = render_turns_for_summary(&turns);
        // Should be truncated to 500 chars + "..."
        assert!(rendered.len() < 1000);
        assert!(rendered.contains("..."));
    }

    #[test]
    fn estimate_token_count_basic() {
        let mut history = History::default();
        history.push(Turn::User {
            content:   "Hello world".into(), // 11 chars
            timestamp: SystemTime::now(),
        });
        // system_prompt = "test" (4 chars) + 11 chars = 15 chars / 4 = 3 tokens
        assert_eq!(estimate_token_count("test", &history), 3);
    }

    #[test]
    fn check_context_usage_below_threshold() {
        let history = History::default();
        let emitter = Emitter::new();
        let profile = TestProfile::new();
        // Empty history, huge context window => well below threshold
        let over = check_context_usage("short", &history, &profile, 80, &emitter, "sess");
        assert!(!over);
    }

    #[test]
    fn check_context_usage_above_threshold() {
        let mut history = History::default();
        // Push enough content to exceed a tiny context window
        history.push(Turn::User {
            content:   "x".repeat(1000),
            timestamp: SystemTime::now(),
        });
        let emitter = Emitter::new();
        let mut rx = emitter.subscribe();
        // TestProfile has context_window=200_000 by default; use a small one
        let profile = TestProfile::with_context_window(ToolRegistry::new(), 100);
        let over = check_context_usage("prompt", &history, &profile, 80, &emitter, "sess");
        assert!(over);

        // Should have emitted a Warning
        let event = rx.try_recv().unwrap();
        assert!(matches!(event.event, AgentEvent::Warning { .. }));
    }
}
