use std::collections::{BTreeMap, HashSet};

use chrono::Utc;
use fabro_llm::token_count::{
    estimate_message_tokens, estimate_request_control_tokens, estimate_text_tokens,
    estimate_tool_definition_tokens, is_local_estimator_warning,
};
use fabro_llm::types::{Request, Role, TokenCounts, Warning as LlmWarning};
use fabro_types::{
    StageContextWindowBreakdownItem, StageContextWindowCategory, StageContextWindowCountMethod,
    StageContextWindowProjection, StageContextWindowStaleness, StageContextWindowWarning,
};

use crate::memory::MemoryDocument;
use crate::skills::{Skill, format_skills_prompt_section};
use crate::tool_registry::{ToolDefinitionWithSource, ToolSource};

#[derive(Clone, Copy)]
pub(crate) struct ContextWindowInput<'a> {
    pub request: &'a Request,
    pub tools: &'a [ToolDefinitionWithSource],
    pub system_prompt: &'a str,
    pub memory: &'a [MemoryDocument],
    pub skills: &'a [Skill],
    pub activated_skill_context_observed: bool,
    pub provider: &'a str,
    pub model: &'a str,
    pub context_window_tokens: usize,
}

#[must_use]
pub(crate) fn build_local_snapshot(input: ContextWindowInput<'_>) -> StageContextWindowProjection {
    let mut builder = BreakdownBuilder::default();
    let mut warnings = Vec::new();

    add_message_breakdown(&mut builder, &mut warnings, &input);
    add_tool_breakdown(&mut builder, input.tools);
    add_request_control_breakdown(&mut builder, &mut warnings, input.request);

    if input.activated_skill_context_observed {
        warnings.push(StageContextWindowWarning {
            code:    "activated_skill_context_counted_as_conversation".to_string(),
            message: "Activated skill instructions are counted as conversation in this version."
                .to_string(),
        });
    }

    builder.into_snapshot(SnapshotMeta {
        provider:              input.provider.to_string(),
        model:                 input.model.to_string(),
        context_window_tokens: u64::try_from(input.context_window_tokens).unwrap_or(u64::MAX),
        count_method:          StageContextWindowCountMethod::LocalEstimate,
        staleness:             StageContextWindowStaleness::Live,
        warnings:              dedupe_warnings_by_code(warnings),
    })
}

/// Collapse a snapshot's warning list to one entry per `code`, preserving
/// insertion order. The per-message estimator already dedupes within a single
/// message — but `build_local_snapshot` walks every message in the request, so
/// the same opaque/media/etc. warning code accumulates one copy per turn that
/// triggered it. The user only needs to be told once.
#[must_use]
fn dedupe_warnings_by_code(
    warnings: Vec<StageContextWindowWarning>,
) -> Vec<StageContextWindowWarning> {
    let mut seen: HashSet<String> = HashSet::new();
    warnings
        .into_iter()
        .filter(|w| seen.insert(w.code.clone()))
        .collect()
}

#[must_use]
pub(crate) fn scaled_snapshot(
    local: &StageContextWindowProjection,
    input_tokens: u64,
    count_method: StageContextWindowCountMethod,
    warnings: Vec<StageContextWindowWarning>,
) -> StageContextWindowProjection {
    let breakdown = scale_breakdown(&local.breakdown, input_tokens, local.context_window_tokens);
    // When the displayed total is provider-authoritative, drop warnings that
    // are only about local-estimator imprecision — they describe the per-
    // category split, not the total the user sees, and tend to alarm users
    // about a number that's actually correct.
    let warnings = if total_is_provider_authoritative(count_method) {
        warnings
            .into_iter()
            .filter(|w| !is_local_estimator_warning(&w.code))
            .collect()
    } else {
        warnings
    };
    let warnings = dedupe_warnings_by_code(warnings);
    StageContextWindowProjection {
        provider: local.provider.clone(),
        model: local.model.clone(),
        context_window_tokens: local.context_window_tokens,
        input_tokens,
        usage_percent: usage_percent(input_tokens, local.context_window_tokens),
        count_method,
        staleness: StageContextWindowStaleness::Live,
        generated_at: Utc::now(),
        event_seq: None,
        breakdown,
        warnings,
    }
}

const fn total_is_provider_authoritative(method: StageContextWindowCountMethod) -> bool {
    matches!(
        method,
        StageContextWindowCountMethod::ProviderApiScaledBreakdown
            | StageContextWindowCountMethod::ResponseUsageScaledBreakdown
    )
}

/// Build a projection from a previously-computed local snapshot and the
/// token usage returned by the LLM response. If the response carried no
/// usable input tokens, fall back to the local estimate unchanged.
#[must_use]
pub(crate) fn context_window_from_response_usage(
    local_snapshot: &StageContextWindowProjection,
    usage: &TokenCounts,
) -> StageContextWindowProjection {
    let input_tokens = usage
        .input_tokens
        .saturating_add(usage.cache_read_tokens)
        .saturating_add(usage.cache_write_tokens);
    if input_tokens <= 0 {
        return local_snapshot.clone();
    }
    scaled_snapshot(
        local_snapshot,
        u64::try_from(input_tokens).unwrap_or(u64::MAX),
        StageContextWindowCountMethod::ResponseUsageScaledBreakdown,
        local_snapshot.warnings.clone(),
    )
}

#[must_use]
fn warnings_from_llm(warnings: &[LlmWarning]) -> Vec<StageContextWindowWarning> {
    warnings
        .iter()
        .map(|warning| StageContextWindowWarning {
            code:    warning
                .code
                .clone()
                .unwrap_or_else(|| "token_count_warning".to_string()),
            message: warning.message.clone(),
        })
        .collect()
}

fn add_message_breakdown(
    builder: &mut BreakdownBuilder,
    warnings: &mut Vec<StageContextWindowWarning>,
    input: &ContextWindowInput<'_>,
) {
    let memory_text = memory_prompt_suffix(input.memory);
    let skills_text = skills_prompt_suffix(input.skills);
    let memory_tokens = estimate_text_tokens(&memory_text);
    let skills_tokens = estimate_text_tokens(&skills_text);
    let mut system_parts_seen = false;

    for message in &input.request.messages {
        let estimate = estimate_message_tokens(message);
        warnings.extend(warnings_from_llm(&estimate.warnings));
        if message.role == Role::System
            && !system_parts_seen
            && message.text() == input.system_prompt
        {
            system_parts_seen = true;
            let attributed_suffix = memory_tokens.saturating_add(skills_tokens);
            builder.add(
                StageContextWindowCategory::SystemPrompt,
                estimate.tokens.saturating_sub(attributed_suffix),
            );
            builder.add(StageContextWindowCategory::Memory, memory_tokens);
            builder.add(StageContextWindowCategory::Skills, skills_tokens);
        } else {
            builder.add(StageContextWindowCategory::Conversation, estimate.tokens);
        }
    }
}

fn add_tool_breakdown(builder: &mut BreakdownBuilder, tools: &[ToolDefinitionWithSource]) {
    for tool in tools {
        let tokens = estimate_tool_definition_tokens(&tool.definition);
        match &tool.source {
            ToolSource::Native => builder.add(StageContextWindowCategory::Tools, tokens),
            ToolSource::Mcp { .. } => builder.add(StageContextWindowCategory::McpTools, tokens),
            ToolSource::Skill => builder.add(StageContextWindowCategory::Skills, tokens),
        }
    }
}

fn add_request_control_breakdown(
    builder: &mut BreakdownBuilder,
    warnings: &mut Vec<StageContextWindowWarning>,
    request: &Request,
) {
    let estimate = estimate_request_control_tokens(request);
    warnings.extend(warnings_from_llm(&estimate.warnings));
    builder.add(StageContextWindowCategory::Other, estimate.tokens);
}

fn memory_prompt_suffix(memory: &[MemoryDocument]) -> String {
    if memory.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n{}",
            memory
                .iter()
                .map(|document| document.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        )
    }
}

fn skills_prompt_suffix(skills: &[Skill]) -> String {
    let section = format_skills_prompt_section(skills);
    if section.is_empty() {
        String::new()
    } else {
        format!("\n\n{section}")
    }
}

#[derive(Default)]
struct BreakdownBuilder {
    tokens: BTreeMap<StageContextWindowCategory, u64>,
}

impl BreakdownBuilder {
    fn add(&mut self, category: StageContextWindowCategory, tokens: usize) {
        if tokens == 0 {
            return;
        }
        let tokens = u64::try_from(tokens).unwrap_or(u64::MAX);
        self.tokens
            .entry(category)
            .and_modify(|existing| *existing = existing.saturating_add(tokens))
            .or_insert(tokens);
    }

    fn into_snapshot(self, meta: SnapshotMeta) -> StageContextWindowProjection {
        let input_tokens = self.tokens.values().copied().sum::<u64>();
        let breakdown = self
            .tokens
            .into_iter()
            .map(|(category, tokens)| StageContextWindowBreakdownItem {
                category,
                tokens,
                usage_percent: usage_percent(tokens, meta.context_window_tokens),
            })
            .collect();
        StageContextWindowProjection {
            provider: meta.provider,
            model: meta.model,
            context_window_tokens: meta.context_window_tokens,
            input_tokens,
            usage_percent: usage_percent(input_tokens, meta.context_window_tokens),
            count_method: meta.count_method,
            staleness: meta.staleness,
            generated_at: Utc::now(),
            event_seq: None,
            breakdown,
            warnings: meta.warnings,
        }
    }
}

struct SnapshotMeta {
    provider:              String,
    model:                 String,
    context_window_tokens: u64,
    count_method:          StageContextWindowCountMethod,
    staleness:             StageContextWindowStaleness,
    warnings:              Vec<StageContextWindowWarning>,
}

/// Proportionally scale a local breakdown so it sums to `target_total`. Any
/// rounding leftover is absorbed by the last bucket; this is a best-effort
/// estimate, not exact apportionment.
fn scale_breakdown(
    breakdown: &[StageContextWindowBreakdownItem],
    target_total: u64,
    context_window_tokens: u64,
) -> Vec<StageContextWindowBreakdownItem> {
    let local_total = breakdown.iter().map(|item| item.tokens).sum::<u64>();
    if breakdown.is_empty() || local_total == 0 {
        return (target_total > 0)
            .then(|| StageContextWindowBreakdownItem {
                category:      StageContextWindowCategory::Other,
                tokens:        target_total,
                usage_percent: usage_percent(target_total, context_window_tokens),
            })
            .into_iter()
            .collect();
    }

    let mut scaled: Vec<_> = breakdown
        .iter()
        .map(|item| {
            let scaled = u128::from(item.tokens).saturating_mul(u128::from(target_total))
                / u128::from(local_total);
            let tokens = u64::try_from(scaled).unwrap_or(u64::MAX);
            StageContextWindowBreakdownItem {
                category: item.category,
                tokens,
                usage_percent: usage_percent(tokens, context_window_tokens),
            }
        })
        .collect();

    // Push any rounding leftover into the last bucket so totals match exactly.
    let allocated: u64 = scaled.iter().map(|item| item.tokens).sum();
    if let Some(last) = scaled.last_mut() {
        let leftover = target_total.saturating_sub(allocated);
        if leftover > 0 {
            last.tokens = last.tokens.saturating_add(leftover);
            last.usage_percent = usage_percent(last.tokens, context_window_tokens);
        }
    }
    scaled
}

fn usage_percent(tokens: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        (tokens as f64) * 100.0 / (denominator as f64)
    }
}

#[cfg(test)]
mod tests {
    use fabro_llm::types::{Message as LlmMessage, Request, ToolChoice, ToolDefinition};

    use super::*;
    use crate::tool_registry::ToolDefinitionWithSource;

    fn request(messages: Vec<LlmMessage>, tools: Vec<ToolDefinition>) -> Request {
        Request {
            model: "model-a".to_string(),
            messages,
            provider: Some("test".to_string()),
            tools: (!tools.is_empty()).then_some(tools),
            tool_choice: Some(ToolChoice::Auto),
            response_format: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop_sequences: None,
            reasoning_effort: None,
            speed: None,
            metadata: None,
            provider_options: None,
        }
    }

    fn tool(name: &str, source: ToolSource) -> ToolDefinitionWithSource {
        ToolDefinitionWithSource {
            definition: ToolDefinition::function(
                name,
                format!("{name} description"),
                serde_json::json!({"type": "object"}),
            ),
            source,
        }
    }

    #[test]
    fn local_breakdown_buckets_system_memory_skills_tools_and_conversation() {
        let memory = vec![MemoryDocument {
            path:         "/repo/AGENTS.md".to_string(),
            content:      "memory instructions".to_string(),
            byte_count:   19,
            loaded_bytes: 19,
            truncated:    false,
        }];
        let skills = vec![Skill {
            name:        "commit".to_string(),
            description: "Commit changes".to_string(),
            template:    "commit template".to_string(),
        }];
        let system_prompt = format!(
            "core prompt{}{}",
            memory_prompt_suffix(&memory),
            skills_prompt_suffix(&skills)
        );
        let tools = vec![
            tool("read_file", ToolSource::Native),
            tool("mcp__server__search", ToolSource::Mcp {
                server_name: "server".to_string(),
            }),
            tool("use_skill", ToolSource::Skill),
        ];
        let req = request(
            vec![
                LlmMessage::system(system_prompt.clone()),
                LlmMessage::user("hello"),
            ],
            tools.iter().map(|tool| tool.definition.clone()).collect(),
        );

        let snapshot = build_local_snapshot(ContextWindowInput {
            request: &req,
            tools: &tools,
            system_prompt: &system_prompt,
            memory: &memory,
            skills: &skills,
            activated_skill_context_observed: true,
            provider: "test",
            model: "model-a",
            context_window_tokens: 100_000,
        });

        let categories = snapshot
            .breakdown
            .iter()
            .map(|item| item.category)
            .collect::<Vec<_>>();
        assert!(categories.contains(&StageContextWindowCategory::SystemPrompt));
        assert!(categories.contains(&StageContextWindowCategory::Memory));
        assert!(categories.contains(&StageContextWindowCategory::Skills));
        assert!(categories.contains(&StageContextWindowCategory::Tools));
        assert!(categories.contains(&StageContextWindowCategory::McpTools));
        assert!(categories.contains(&StageContextWindowCategory::Conversation));
        assert_eq!(
            snapshot
                .breakdown
                .iter()
                .map(|item| item.tokens)
                .sum::<u64>(),
            snapshot.input_tokens
        );
        assert!(
            snapshot.warnings.iter().any(|warning| {
                warning.code == "activated_skill_context_counted_as_conversation"
            })
        );
    }

    #[test]
    fn scaled_breakdown_totals_provider_count() {
        let local = StageContextWindowProjection {
            provider:              "test".to_string(),
            model:                 "model-a".to_string(),
            context_window_tokens: 1000,
            input_tokens:          30,
            usage_percent:         3.0,
            count_method:          StageContextWindowCountMethod::LocalEstimate,
            staleness:             StageContextWindowStaleness::Live,
            generated_at:          Utc::now(),
            event_seq:             None,
            breakdown:             vec![
                StageContextWindowBreakdownItem {
                    category:      StageContextWindowCategory::SystemPrompt,
                    tokens:        10,
                    usage_percent: 0.0,
                },
                StageContextWindowBreakdownItem {
                    category:      StageContextWindowCategory::Conversation,
                    tokens:        20,
                    usage_percent: 0.0,
                },
            ],
            warnings:              Vec::new(),
        };

        let scaled = scaled_snapshot(
            &local,
            101,
            StageContextWindowCountMethod::ProviderApiScaledBreakdown,
            Vec::new(),
        );

        assert_eq!(scaled.input_tokens, 101);
        assert_eq!(
            scaled.breakdown.iter().map(|item| item.tokens).sum::<u64>(),
            101
        );
    }

    /// Build a minimal snapshot with one estimator-noise warning and one
    /// semantic warning, used by the warning-suppression assertions below.
    fn snapshot_for_warning_test() -> StageContextWindowProjection {
        StageContextWindowProjection {
            provider:              "test".to_string(),
            model:                 "model-a".to_string(),
            context_window_tokens: 1000,
            input_tokens:          50,
            usage_percent:         5.0,
            count_method:          StageContextWindowCountMethod::LocalEstimate,
            staleness:             StageContextWindowStaleness::Live,
            generated_at:          Utc::now(),
            event_seq:             None,
            breakdown:             vec![StageContextWindowBreakdownItem {
                category:      StageContextWindowCategory::Conversation,
                tokens:        50,
                usage_percent: 5.0,
            }],
            warnings:              Vec::new(),
        }
    }

    fn warnings_in() -> Vec<StageContextWindowWarning> {
        use fabro_llm::token_count::{MEDIA_ESTIMATE_WARNING, OPAQUE_CONTEXT_ESTIMATE_WARNING};
        vec![
            StageContextWindowWarning {
                code:    OPAQUE_CONTEXT_ESTIMATE_WARNING.to_string(),
                message: "noise".to_string(),
            },
            StageContextWindowWarning {
                code:    MEDIA_ESTIMATE_WARNING.to_string(),
                message: "noise".to_string(),
            },
            StageContextWindowWarning {
                code:    "activated_skill_context_counted_as_conversation".to_string(),
                message: "kept".to_string(),
            },
        ]
    }

    #[test]
    fn scaled_snapshot_drops_estimator_noise_when_total_is_provider_authoritative() {
        let local = snapshot_for_warning_test();
        let scaled = scaled_snapshot(
            &local,
            100,
            StageContextWindowCountMethod::ProviderApiScaledBreakdown,
            warnings_in(),
        );
        let codes: Vec<_> = scaled.warnings.iter().map(|w| w.code.as_str()).collect();
        assert_eq!(codes, vec![
            "activated_skill_context_counted_as_conversation"
        ]);
    }

    #[test]
    fn scaled_snapshot_drops_estimator_noise_under_response_usage_scaling() {
        let local = snapshot_for_warning_test();
        let scaled = scaled_snapshot(
            &local,
            100,
            StageContextWindowCountMethod::ResponseUsageScaledBreakdown,
            warnings_in(),
        );
        let codes: Vec<_> = scaled.warnings.iter().map(|w| w.code.as_str()).collect();
        assert_eq!(codes, vec![
            "activated_skill_context_counted_as_conversation"
        ]);
    }

    #[test]
    fn scaled_snapshot_keeps_estimator_warnings_for_local_estimate() {
        let local = snapshot_for_warning_test();
        let scaled = scaled_snapshot(
            &local,
            100,
            StageContextWindowCountMethod::LocalEstimate,
            warnings_in(),
        );
        let codes: Vec<_> = scaled.warnings.iter().map(|w| w.code.as_str()).collect();
        // When the total itself is locally estimated, the estimator-noise
        // warnings remain meaningful and must surface.
        assert_eq!(codes, vec![
            "opaque_context_estimate",
            "media_token_estimate",
            "activated_skill_context_counted_as_conversation",
        ]);
    }

    #[test]
    fn scaled_snapshot_dedupes_repeated_warning_codes() {
        use fabro_llm::token_count::OPAQUE_CONTEXT_ESTIMATE_WARNING;
        let local = snapshot_for_warning_test();
        // Simulate the real bug: build_local_snapshot walks N messages and
        // adds the same `opaque_context_estimate` warning once per turn that
        // had an opaque block, so a long conversation accumulates many copies.
        let repeated: Vec<_> = (0..5)
            .map(|i| StageContextWindowWarning {
                code:    OPAQUE_CONTEXT_ESTIMATE_WARNING.to_string(),
                message: format!("turn {i}"),
            })
            .collect();

        let scaled = scaled_snapshot(
            &local,
            100,
            StageContextWindowCountMethod::LocalEstimate,
            repeated,
        );

        let codes: Vec<_> = scaled.warnings.iter().map(|w| w.code.as_str()).collect();
        assert_eq!(codes, vec!["opaque_context_estimate"]);
    }
}
