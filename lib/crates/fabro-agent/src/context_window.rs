use std::collections::BTreeMap;

use chrono::Utc;
use fabro_llm::token_count::{
    estimate_message_tokens, estimate_request_control_tokens, estimate_text_tokens,
    estimate_tool_definition_tokens,
};
use fabro_llm::types::{Request, Role, Warning as LlmWarning};
use fabro_types::{
    StageContextWindowBreakdownProjection, StageContextWindowCategory,
    StageContextWindowCountMethod, StageContextWindowProjection, StageContextWindowStaleness,
    StageContextWindowWarning,
};

use crate::memory::MemoryDocument;
use crate::skills::{Skill, format_skills_prompt_section};
use crate::tool_registry::{ToolDefinitionWithSource, ToolSource};

const LOCAL_BREAKDOWN_SOURCE: &str = "local_estimate";
const SCALED_BREAKDOWN_SOURCE: &str = "scaled_local_estimate";

#[derive(Clone, Copy)]
pub(crate) struct ContextWindowSnapshotInput<'a> {
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
pub(crate) fn build_local_snapshot(
    input: ContextWindowSnapshotInput<'_>,
) -> StageContextWindowProjection {
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
        provider: input.provider.to_string(),
        model: input.model.to_string(),
        context_window_tokens: u64::try_from(input.context_window_tokens).unwrap_or(u64::MAX),
        count_method: StageContextWindowCountMethod::LocalEstimate,
        staleness: StageContextWindowStaleness::Live,
        breakdown_source: LOCAL_BREAKDOWN_SOURCE,
        warnings,
    })
}

#[must_use]
pub(crate) fn scaled_snapshot(
    local: &StageContextWindowProjection,
    input_tokens: u64,
    count_method: StageContextWindowCountMethod,
    warnings: Vec<StageContextWindowWarning>,
) -> StageContextWindowProjection {
    let breakdown = scale_breakdown(&local.breakdown, input_tokens, local.context_window_tokens);
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

#[must_use]
pub(crate) fn warnings_from_llm(warnings: &[LlmWarning]) -> Vec<StageContextWindowWarning> {
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

#[must_use]
pub(crate) fn warning(code: &str, message: &str) -> StageContextWindowWarning {
    StageContextWindowWarning {
        code:    code.to_string(),
        message: message.to_string(),
    }
}

fn add_message_breakdown(
    builder: &mut BreakdownBuilder,
    warnings: &mut Vec<StageContextWindowWarning>,
    input: &ContextWindowSnapshotInput<'_>,
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
            .map(|(category, tokens)| StageContextWindowBreakdownProjection {
                category,
                label: category_label(category).to_string(),
                tokens,
                usage_percent: usage_percent(tokens, meta.context_window_tokens),
                source: meta.breakdown_source.to_string(),
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
    breakdown_source:      &'static str,
    warnings:              Vec<StageContextWindowWarning>,
}

fn scale_breakdown(
    breakdown: &[StageContextWindowBreakdownProjection],
    target_total: u64,
    context_window_tokens: u64,
) -> Vec<StageContextWindowBreakdownProjection> {
    let local_total = breakdown.iter().map(|item| item.tokens).sum::<u64>();
    if target_total == local_total {
        return breakdown
            .iter()
            .cloned()
            .map(|mut item| {
                item.source = SCALED_BREAKDOWN_SOURCE.to_string();
                item
            })
            .collect();
    }
    if breakdown.is_empty() || local_total == 0 {
        return (target_total > 0)
            .then(|| StageContextWindowBreakdownProjection {
                category:      StageContextWindowCategory::Other,
                label:         category_label(StageContextWindowCategory::Other).to_string(),
                tokens:        target_total,
                usage_percent: usage_percent(target_total, context_window_tokens),
                source:        SCALED_BREAKDOWN_SOURCE.to_string(),
            })
            .into_iter()
            .collect();
    }

    let mut scaled = Vec::with_capacity(breakdown.len());
    let mut allocated = 0u64;
    let mut remainders = Vec::with_capacity(breakdown.len());
    let local_total_u128 = u128::from(local_total);
    for (index, item) in breakdown.iter().enumerate() {
        let numerator = u128::from(item.tokens).saturating_mul(u128::from(target_total));
        let tokens = u64::try_from(numerator / local_total_u128).unwrap_or(u64::MAX);
        allocated = allocated.saturating_add(tokens);
        remainders.push((index, numerator % local_total_u128));
        scaled.push(StageContextWindowBreakdownProjection {
            category: item.category,
            label: item.label.clone(),
            tokens,
            usage_percent: 0.0,
            source: SCALED_BREAKDOWN_SOURCE.to_string(),
        });
    }

    let mut remaining = target_total.saturating_sub(allocated);
    remainders.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    for (index, _) in remainders {
        if remaining == 0 {
            break;
        }
        scaled[index].tokens = scaled[index].tokens.saturating_add(1);
        remaining -= 1;
    }
    for item in &mut scaled {
        item.usage_percent = usage_percent(item.tokens, context_window_tokens);
    }
    scaled
}

fn category_label(category: StageContextWindowCategory) -> &'static str {
    match category {
        StageContextWindowCategory::SystemPrompt => "System prompt",
        StageContextWindowCategory::Tools => "Tools",
        StageContextWindowCategory::McpTools => "MCP tools",
        StageContextWindowCategory::Skills => "Skills",
        StageContextWindowCategory::Memory => "Memory",
        StageContextWindowCategory::Conversation => "Conversation",
        StageContextWindowCategory::Other => "Other",
    }
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

        let snapshot = build_local_snapshot(ContextWindowSnapshotInput {
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
                StageContextWindowBreakdownProjection {
                    category:      StageContextWindowCategory::SystemPrompt,
                    label:         "System prompt".to_string(),
                    tokens:        10,
                    usage_percent: 0.0,
                    source:        LOCAL_BREAKDOWN_SOURCE.to_string(),
                },
                StageContextWindowBreakdownProjection {
                    category:      StageContextWindowCategory::Conversation,
                    label:         "Conversation".to_string(),
                    tokens:        20,
                    usage_percent: 0.0,
                    source:        LOCAL_BREAKDOWN_SOURCE.to_string(),
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
        assert!(
            scaled
                .breakdown
                .iter()
                .all(|item| item.source == SCALED_BREAKDOWN_SOURCE)
        );
    }
}
