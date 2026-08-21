use std::fmt::Write;

use fabro_llm::client::Client;
use fabro_llm::types::{Message as LlmMessage, Request};
use fabro_model::{RunBudget, UsdMicros};
use tracing::debug;

use crate::agent_profile::AgentProfile;
use crate::error::{CompactionError, Error};
use crate::event::Emitter;
use crate::file_tracker::FileTracker;
use crate::history::History;
use crate::types::{AgentEvent, Message};

const APPROX_CHARS_PER_TOKEN: usize = 4;

/// Maximum output budget for the visible summary text itself.
const SUMMARY_MAX_TOKENS: i64 = 4096;

/// Extra output budget for models that reason on every request. `max_tokens`
/// bounds reasoning *plus* visible output, so a reasoning model handed only
/// `SUMMARY_MAX_TOKENS` can spend the whole budget thinking and return a
/// successful response with empty content — a silently empty summary.
const REASONING_HEADROOM_TOKENS: i64 = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum ContextEstimateMethod {
    ApiUsagePlusLocalDelta,
    LocalEstimate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContextEstimate {
    pub tokens: usize,
    pub method: ContextEstimateMethod,
}

/// Check whether the context window usage exceeds the configured threshold.
/// Emits a `Warning` event with kind `"context_window"` when over the
/// threshold. Returns `Some(estimate)` if the threshold is exceeded so the
/// caller can pass it to `compact_context` without recomputing.
pub(crate) fn check_context_usage(
    system_prompt: &str,
    history: &History,
    provider_profile: &dyn AgentProfile,
    threshold_percent: usize,
    emitter: &Emitter,
    session_id: &str,
) -> Option<ContextEstimate> {
    let estimate = estimate_active_context_usage(system_prompt, history);
    let context_window = provider_profile.context_window_size();
    let threshold = context_window * threshold_percent / 100;

    if estimate.tokens > threshold {
        let usage_percent = estimate.tokens.saturating_mul(100) / context_window;
        let method: &'static str = estimate.method.into();
        emitter.emit(session_id.to_owned(), AgentEvent::Warning {
            kind:    "context_window".into(),
            message: format!("Context window usage: {usage_percent}%"),
            details: serde_json::json!({
                "estimated_tokens": estimate.tokens,
                "context_window_size": context_window,
                "usage_percent": usage_percent,
                "estimate_method": method,
            }),
        });
        Some(estimate)
    } else {
        None
    }
}

/// Compact the conversation history by summarizing older turns via a
/// non-streaming LLM call.
#[allow(
    clippy::too_many_arguments,
    reason = "Context compaction needs explicit history, model, tracking, and emission inputs."
)]
pub(crate) async fn compact_context(
    history: &mut History,
    llm_client: &Client,
    provider_profile: &dyn AgentProfile,
    file_tracker: &FileTracker,
    preserve_count: usize,
    estimate: ContextEstimate,
    emitter: &Emitter,
    session_id: &str,
    run_budget: Option<&RunBudget>,
) -> Result<(), Error> {
    let original_turn_count = history.turns().len();
    let preserve_start = history.compact_preserve_start(preserve_count);

    // If preserving tool call/result pairs leaves no prefix to summarize, do
    // not spend a summarization call or emit a started event without a
    // matching completion.
    if preserve_start == 0 {
        return Ok(());
    }
    let preserved_turn_count = original_turn_count - preserve_start;

    emitter.emit(session_id.to_owned(), AgentEvent::CompactionStarted {
        estimated_tokens:    estimate.tokens,
        context_window_size: provider_profile.context_window_size(),
    });

    let turns_to_summarize = &history.turns()[..preserve_start];
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

    let max_tokens = summary_max_tokens(
        provider_profile.reasons_by_default(),
        provider_profile.max_output_tokens(),
    );
    let visible_max_tokens = SUMMARY_MAX_TOKENS.min(max_tokens);

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
Keep the entire response under {visible_max_tokens} tokens.\n\n\
Be thorough and specific — the assistant taking over has no prior context. Include file paths, \
function names, error messages, and exact values. Omit pleasantries and conversational filler.\
{file_ops_section}"
    );

    let summary_request = Request {
        model:            provider_profile.model().to_string(),
        messages:         vec![
            LlmMessage::system(summarization_prompt),
            LlmMessage::user(format!(
                "Here is the conversation to summarize:\n\n{rendered}"
            )),
        ],
        provider:         Some(provider_profile.provider_id().to_string()),
        tools:            None,
        tool_choice:      None,
        response_format:  None,
        temperature:      None,
        top_p:            None,
        max_tokens:       Some(max_tokens),
        stop_sequences:   None,
        reasoning_effort: None,
        speed:            None,
        metadata:         None,
        provider_options: None,
    };

    let response = llm_client
        .complete(&summary_request)
        .await
        .map_err(CompactionError::Llm)?;

    // Summarization spend counts against the run budget. A trip here is only
    // charged; the session's next per-turn check halts it.
    if let Some(budget) = run_budget {
        let _ = budget.charge(
            response.usage.total_tokens(),
            response.cost_usd.map(UsdMicros::from_usd),
        );
    }

    let response_text = response.text();
    let summary_text = response_text.trim();

    // `compact_from` discards summarized turns irreversibly. Refuse an empty
    // response before mutating history; trimming also prevents a
    // whitespace-only response from masquerading as a summary.
    if summary_text.is_empty() {
        return Err(CompactionError::EmptySummary {
            summarized_turn_count: preserve_start,
        }
        .into());
    }

    let (summary_text, summary_truncated) = truncate_summary_text(summary_text);
    debug!(
        summary_len = summary_text.len(),
        summary_truncated, max_tokens, "Compaction summary generated"
    );
    let summary_content = format!(
        "A different assistant began this task and produced the following summary. \
Build on their progress — do not repeat completed steps.\n\n{summary_text}"
    );
    let summary_token_estimate = estimate_chars_local_tokens(summary_content.len());

    history.compact_from(preserve_start, summary_content);

    emitter.emit(session_id.to_owned(), AgentEvent::CompactionCompleted {
        original_turn_count,
        preserved_turn_count,
        summary_token_estimate,
        tracked_file_count: file_tracker.file_count(),
    });

    Ok(())
}

/// Combined reasoning and visible-output budget for the summarization request.
///
/// Compaction runs against the session's own model, so a reasoning session
/// summarizes with reasoning enabled and the budget has to cover the thinking
/// as well as the summary. Provider routes that reason by default get headroom
/// on top of the summary allowance. Every known model budget is capped at its
/// declared `max_output`.
fn summary_max_tokens(reasoning_by_default: bool, max_output: Option<i64>) -> i64 {
    let budget = if reasoning_by_default {
        SUMMARY_MAX_TOKENS + REASONING_HEADROOM_TOKENS
    } else {
        SUMMARY_MAX_TOKENS
    };

    max_output.map_or(budget, |limit| budget.min(limit))
}

/// Bound retained summary text with the same local bytes-per-token heuristic
/// used for context estimates. Provider APIs expose only one combined ceiling
/// for reasoning and visible output, so the larger request budget cannot
/// enforce this limit itself.
fn truncate_summary_text(summary: &str) -> (&str, bool) {
    let max_bytes = summary_max_approx_bytes();
    if summary.len() <= max_bytes {
        return (summary, false);
    }

    let end = summary.floor_char_boundary(max_bytes);
    (&summary[..end], true)
}

fn summary_max_approx_bytes() -> usize {
    usize::try_from(SUMMARY_MAX_TOKENS)
        .unwrap_or(usize::MAX)
        .saturating_mul(APPROX_CHARS_PER_TOKEN)
}

pub(crate) fn estimate_active_context_usage(
    system_prompt: &str,
    history: &History,
) -> ContextEstimate {
    let turns = history.turns();
    if let Some((baseline_index, baseline_tokens)) = latest_assistant_usage_baseline(turns) {
        let local_delta = estimate_turns_local_tokens(&turns[baseline_index + 1..]);
        return ContextEstimate {
            tokens: baseline_tokens.saturating_add(local_delta),
            method: ContextEstimateMethod::ApiUsagePlusLocalDelta,
        };
    }

    ContextEstimate {
        tokens: estimate_chars_local_tokens(
            system_prompt
                .len()
                .saturating_add(estimate_turns_local_chars(turns)),
        ),
        method: ContextEstimateMethod::LocalEstimate,
    }
}

fn latest_assistant_usage_baseline(turns: &[Message]) -> Option<(usize, usize)> {
    turns.iter().enumerate().rev().find_map(|(index, turn)| {
        if let Message::Assistant { usage, .. } = turn {
            let total_tokens = usage.total_tokens();
            if total_tokens > 0 {
                return Some((index, usize::try_from(total_tokens).unwrap_or(usize::MAX)));
            }
        }
        None
    })
}

fn estimate_turns_local_tokens(turns: &[Message]) -> usize {
    estimate_chars_local_tokens(estimate_turns_local_chars(turns))
}

fn estimate_turns_local_chars(turns: &[Message]) -> usize {
    turns.iter().fold(0usize, |total, turn| {
        total.saturating_add(estimate_turn_chars(turn))
    })
}

fn estimate_chars_local_tokens(chars: usize) -> usize {
    chars / APPROX_CHARS_PER_TOKEN
}

fn estimate_turn_chars(turn: &Message) -> usize {
    match turn {
        Message::User { content, .. }
        | Message::System { content, .. }
        | Message::Steering { content, .. } => content.len(),
        Message::Assistant {
            content,
            tool_calls,
            ..
        } => {
            let reasoning_chars = turn.reasoning_text().map_or(0, str::len);
            let tool_call_chars: usize = tool_calls
                .iter()
                .map(|tc| tc.name.len() + tc.arguments.to_string().len())
                .sum();
            content.len() + reasoning_chars + tool_call_chars
        }
        Message::ToolResults { results, .. } => {
            results.iter().map(|r| r.content.to_string().len()).sum()
        }
    }
}

/// Render conversation turns into a human-readable summary format for the
/// compaction LLM call.
pub fn render_turns_for_summary(turns: &[Message]) -> String {
    let mut out = String::new();
    for turn in turns {
        match turn {
            Message::User { content, .. } => {
                let _ = writeln!(out, "User: {content}");
            }
            Message::Assistant {
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
                        format!("{}...", &args_str[..args_str.floor_char_boundary(500)])
                    } else {
                        args_str
                    };
                    let _ = writeln!(out, "[Tool call: {}] {truncated}", tc.name);
                }
            }
            Message::ToolResults { results, .. } => {
                for r in results {
                    let content_str = r.content.to_string();
                    let truncated = if content_str.len() > 500 {
                        format!(
                            "{}...",
                            &content_str[..content_str.floor_char_boundary(500)]
                        )
                    } else {
                        content_str
                    };
                    let _ = writeln!(out, "[Tool result: {}] {truncated}", r.tool_call_id);
                }
            }
            Message::System { content, .. } => {
                let _ = writeln!(out, "System: {content}");
            }
            Message::Steering { content, .. } => {
                let _ = writeln!(out, "Steering: {content}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::SystemTime;

    use fabro_llm::types::{TokenCounts, ToolCall, ToolResult};
    use fabro_model::{Catalog, Model, ProviderId};

    use super::*;
    use crate::event::Emitter;
    use crate::history::History;
    use crate::test_support::{MockLlmProvider, TestProfile, make_client, text_response};
    use crate::tool_registry::ToolRegistry;
    use crate::types::Message;

    fn catalog_model(provider: &ProviderId, id: &str) -> &'static Model {
        Catalog::builtin()
            .get_on_provider(provider, id)
            .unwrap_or_else(|| panic!("{provider}/{id} missing from builtin catalog"))
    }

    fn builtin_summary_max_tokens(provider: &ProviderId, id: &str) -> i64 {
        let catalog = Catalog::builtin();
        let model = catalog_model(provider, id);
        let settings = catalog
            .settings_for(model)
            .unwrap_or_else(|| panic!("{provider}/{id} missing catalog settings"));
        summary_max_tokens(settings.reasoning_by_default, model.max_output())
    }

    #[test]
    fn summary_budget_without_catalog_model_is_summary_allowance() {
        assert_eq!(summary_max_tokens(false, None), SUMMARY_MAX_TOKENS);
        // The default agent test profile has no catalog behind it.
        let profile = TestProfile::new();
        assert_eq!(
            summary_max_tokens(profile.reasons_by_default(), profile.max_output_tokens()),
            SUMMARY_MAX_TOKENS
        );
    }

    #[test]
    fn summary_budget_for_non_reasoning_model_is_summary_allowance() {
        // claude-haiku-4-5: reasoning = false.
        assert_eq!(
            builtin_summary_max_tokens(&ProviderId::anthropic(), "claude-haiku-4-5"),
            SUMMARY_MAX_TOKENS
        );
    }

    #[test]
    fn summary_budget_for_model_without_effort_feature_is_summary_allowance() {
        // claude-sonnet-4-5 reasons only when a request asks for it, and
        // compaction never sends a reasoning effort.
        let model = catalog_model(&ProviderId::anthropic(), "claude-sonnet-4-5");
        assert!(model.supports_reasoning());
        assert!(!model.supports_reasoning_effort());
        assert_eq!(
            builtin_summary_max_tokens(&ProviderId::anthropic(), "claude-sonnet-4-5"),
            SUMMARY_MAX_TOKENS
        );
    }

    #[test]
    fn summary_budget_for_always_adaptive_model_adds_reasoning_headroom() {
        assert_eq!(
            builtin_summary_max_tokens(&ProviderId::anthropic(), "claude-fable-5"),
            SUMMARY_MAX_TOKENS + REASONING_HEADROOM_TOKENS
        );
    }

    #[test]
    fn summary_budget_for_effort_levels_model_adds_reasoning_headroom() {
        assert_eq!(
            builtin_summary_max_tokens(&ProviderId::anthropic(), "claude-opus-5"),
            SUMMARY_MAX_TOKENS + REASONING_HEADROOM_TOKENS
        );
    }

    #[test]
    fn summary_budget_for_always_reasoning_route_without_effort_adds_headroom() {
        let moonshot = ProviderId::new("moonshot");
        let model = catalog_model(&moonshot, "kimi-k2.5");
        assert!(model.supports_reasoning());
        assert!(!model.supports_reasoning_effort());
        assert_eq!(
            builtin_summary_max_tokens(&moonshot, "kimi-k2.5"),
            SUMMARY_MAX_TOKENS + REASONING_HEADROOM_TOKENS
        );
    }

    #[test]
    fn summary_budget_never_exceeds_model_max_output() {
        assert_eq!(summary_max_tokens(true, Some(8_192)), 8_192);
        assert_eq!(summary_max_tokens(false, Some(2_048)), 2_048);
    }

    #[test]
    fn render_turns_produces_labeled_text() {
        let turns = vec![
            Message::User {
                content:   "Hello".into(),
                timestamp: SystemTime::now(),
            },
            Message::Assistant {
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
            Message::ToolResults {
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
        let turns = vec![Message::ToolResults {
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
    fn estimate_local_token_count_basic() {
        let mut history = History::default();
        history.push(Message::User {
            content:   "Hello world".into(), // 11 chars
            timestamp: SystemTime::now(),
        });
        // system_prompt = "test" (4/4 = 1 token) + 11 chars / 4 = 2 tokens = 3 tokens
        let estimate = estimate_active_context_usage("test", &history);
        assert_eq!(estimate.tokens, 3);
        assert_eq!(estimate.method, ContextEstimateMethod::LocalEstimate);
    }

    #[test]
    fn active_context_estimate_without_assistant_usage_uses_local_estimate() {
        let mut history = History::default();
        history.push(Message::User {
            content:   "Hello world".into(), // 11 chars => 2 tokens
            timestamp: SystemTime::now(),
        });
        history.push(Message::Assistant {
            // 18 chars content + tool call name (9) + args (16) = 43 chars => 10 tokens
            content:        "No usage available".into(),
            tool_calls:     vec![ToolCall::new(
                "call_1",
                "read_file",
                serde_json::json!({"path": "foo.rs"}),
            )],
            provider_parts: vec![],
            usage:          Box::new(TokenCounts::default()),
            response_id:    "resp_1".into(),
            timestamp:      SystemTime::now(),
        });
        history.push(Message::ToolResults {
            // 4 chars => 1 token
            results:   vec![ToolResult::success("call_1", serde_json::json!(1234))],
            timestamp: SystemTime::now(),
        });

        let estimate = estimate_active_context_usage("test", &history);

        assert_eq!(estimate.method, ContextEstimateMethod::LocalEstimate);
        // (system prompt 4 + turn chars 11 + 18 + 9 + 16 + 4) / 4 = 62/4 = 15
        assert_eq!(estimate.tokens, 15);
    }

    #[test]
    fn active_context_local_estimate_matches_whole_history_rounding() {
        let mut history = History::default();
        history.push(Message::User {
            content:   "abc".into(),
            timestamp: SystemTime::now(),
        });

        let estimate = estimate_active_context_usage("x", &history);

        assert_eq!(estimate.method, ContextEstimateMethod::LocalEstimate);
        assert_eq!(estimate.tokens, 1);
    }

    #[test]
    fn active_context_estimate_uses_latest_assistant_usage_plus_later_turns() {
        let mut history = History::default();
        history.push(Message::User {
            content:   "ignored before baseline".repeat(100),
            timestamp: SystemTime::now(),
        });
        history.push(Message::Assistant {
            content:        "baseline response".into(),
            tool_calls:     vec![],
            provider_parts: vec![],
            usage:          Box::new(TokenCounts {
                input_tokens: 50,
                ..TokenCounts::default()
            }),
            response_id:    "resp_1".into(),
            timestamp:      SystemTime::now(),
        });
        history.push(Message::ToolResults {
            // JSON number renders as 4 chars => 1 local token.
            results:   vec![ToolResult::success("call_1", serde_json::json!(1234))],
            timestamp: SystemTime::now(),
        });
        history.push(Message::User {
            // 16 chars => 4 local tokens.
            content:   "u".repeat(16),
            timestamp: SystemTime::now(),
        });
        history.push(Message::Steering {
            // 8 chars => 2 local tokens.
            content:   "s".repeat(8),
            timestamp: SystemTime::now(),
        });

        let estimate = estimate_active_context_usage("ignored system prompt", &history);

        assert_eq!(estimate.tokens, 57);
        assert_eq!(
            estimate.method,
            ContextEstimateMethod::ApiUsagePlusLocalDelta
        );
    }

    #[test]
    fn active_context_estimate_uses_total_tokens_including_cache_and_reasoning() {
        let mut history = History::default();
        history.push(Message::Assistant {
            content:        "short".into(),
            tool_calls:     vec![],
            provider_parts: vec![],
            usage:          Box::new(TokenCounts {
                input_tokens:       10,
                output_tokens:      20,
                reasoning_tokens:   30,
                cache_read_tokens:  40,
                cache_write_tokens: 50,
            }),
            response_id:    "resp_1".into(),
            timestamp:      SystemTime::now(),
        });

        let estimate = estimate_active_context_usage("", &history);

        assert_eq!(estimate.tokens, 150);
        assert_eq!(
            estimate.method,
            ContextEstimateMethod::ApiUsagePlusLocalDelta
        );
    }

    #[test]
    fn active_context_estimate_ignores_earlier_usage_when_later_usage_exists() {
        let mut history = History::default();
        history.push(Message::Assistant {
            content:        "older response".into(),
            tool_calls:     vec![],
            provider_parts: vec![],
            usage:          Box::new(TokenCounts {
                input_tokens: 1_000,
                ..TokenCounts::default()
            }),
            response_id:    "resp_old".into(),
            timestamp:      SystemTime::now(),
        });
        history.push(Message::User {
            content:   "ignored before latest baseline".repeat(100),
            timestamp: SystemTime::now(),
        });
        history.push(Message::Assistant {
            content:        "latest response".into(),
            tool_calls:     vec![],
            provider_parts: vec![],
            usage:          Box::new(TokenCounts {
                input_tokens: 20,
                ..TokenCounts::default()
            }),
            response_id:    "resp_new".into(),
            timestamp:      SystemTime::now(),
        });
        history.push(Message::User {
            content:   "u".repeat(8),
            timestamp: SystemTime::now(),
        });

        let estimate = estimate_active_context_usage("", &history);

        assert_eq!(estimate.tokens, 22);
        assert_eq!(
            estimate.method,
            ContextEstimateMethod::ApiUsagePlusLocalDelta
        );
    }

    #[test]
    fn check_context_usage_below_threshold() {
        let history = History::default();
        let emitter = Emitter::new();
        let profile = TestProfile::new();
        // Empty history, huge context window => well below threshold
        let result = check_context_usage("short", &history, &profile, 80, &emitter, "sess");
        assert!(result.is_none());
    }

    #[test]
    fn check_context_usage_above_threshold() {
        let mut history = History::default();
        // Push enough content to exceed a tiny context window
        history.push(Message::User {
            content:   "x".repeat(1000),
            timestamp: SystemTime::now(),
        });
        let emitter = Emitter::new();
        let mut rx = emitter.subscribe();
        // TestProfile has context_window=200_000 by default; use a small one
        let profile = TestProfile::with_context_window(ToolRegistry::new(), 100);
        let result = check_context_usage("prompt", &history, &profile, 80, &emitter, "sess");
        assert!(result.is_some());

        // Should have emitted a Warning
        let event = rx.try_recv().unwrap();
        assert!(matches!(event.event, AgentEvent::Warning { details, .. }
                if details["estimate_method"] == "local_estimate"));
    }

    struct CompactionTestResult {
        result:         Result<(), Error>,
        history:        History,
        original_turns: Vec<fabro_types::SessionMessage>,
        events:         Vec<AgentEvent>,
    }

    /// Run `compact_context` over a fixed four-turn history against a mock
    /// provider that returns `summary` from the summarization call.
    async fn compact_with_summary(summary: &str) -> CompactionTestResult {
        let mut history = History::default();
        for index in 0..4 {
            history.push(Message::User {
                content:   format!("message {index}"),
                timestamp: SystemTime::now(),
            });
        }
        let original_turns = history.to_session_messages();

        let provider = Arc::new(MockLlmProvider::new(vec![text_response(summary)]));
        let client = make_client(provider).await;
        let profile = TestProfile::new();
        let file_tracker = FileTracker::default();
        let emitter = Emitter::new();
        let mut rx = emitter.subscribe();

        let result = compact_context(
            &mut history,
            &client,
            &profile,
            &file_tracker,
            1,
            ContextEstimate {
                tokens: 1_000,
                method: ContextEstimateMethod::LocalEstimate,
            },
            &emitter,
            "sess",
            None,
        )
        .await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event.event);
        }

        CompactionTestResult {
            result,
            history,
            original_turns,
            events,
        }
    }

    fn assert_history_untouched(history: &History, original_turns: &[fabro_types::SessionMessage]) {
        assert_eq!(
            history.to_session_messages(),
            original_turns,
            "history must remain exactly unchanged when the summary is rejected"
        );
    }

    #[tokio::test]
    async fn compaction_refuses_to_truncate_on_blank_summary() {
        for summary in ["", "   \n\t  \n "] {
            let CompactionTestResult {
                result,
                history,
                original_turns,
                events,
            } = compact_with_summary(summary).await;

            let err = result.expect_err("blank summary must not report success");
            assert!(
                matches!(
                    &err,
                    Error::Compaction(CompactionError::EmptySummary {
                        summarized_turn_count: 3,
                    })
                ),
                "unexpected error: {err}"
            );

            assert_history_untouched(&history, &original_turns);
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, AgentEvent::CompactionStarted { .. })),
                "CompactionStarted should record the attempted summary request"
            );
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, AgentEvent::CompactionCompleted { .. })),
                "CompactionCompleted must not be emitted for a rejected summary"
            );
        }
    }

    #[tokio::test]
    async fn compaction_accepts_concise_nonempty_summary() {
        let CompactionTestResult {
            result,
            history,
            events,
            ..
        } = compact_with_summary("Brief handoff.").await;

        result.expect("a nonempty summary should compact");

        let summary_turn = history
            .turns()
            .iter()
            .find_map(|turn| match turn {
                Message::System { content, .. } => Some(content),
                _ => None,
            })
            .expect("compacted history should contain a summary turn");
        assert!(summary_turn.contains("A different assistant began this task"));
        assert!(summary_turn.contains("Brief handoff."));

        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::CompactionCompleted { .. })),
            "CompactionCompleted should be emitted on success"
        );
    }

    #[tokio::test]
    async fn compaction_bounds_retained_summary_to_visible_budget() {
        let max_bytes = summary_max_approx_bytes();
        let overlong = format!("{}END", "€".repeat(max_bytes / 3 + 1));
        let CompactionTestResult {
            result, history, ..
        } = compact_with_summary(&overlong).await;

        result.expect("an overlong summary should be compacted after truncation");

        let summary_turn = history
            .turns()
            .iter()
            .find_map(|turn| match turn {
                Message::System { content, .. } => Some(content),
                _ => None,
            })
            .expect("compacted history should contain a summary turn");
        let (_, retained_summary) = summary_turn
            .split_once("\n\n")
            .expect("summary turn should separate its header from the generated text");
        assert!(retained_summary.len() <= max_bytes);
        assert!(!retained_summary.contains("END"));
    }
}
