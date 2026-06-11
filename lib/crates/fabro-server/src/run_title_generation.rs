use std::collections::HashMap;
use std::sync::Arc;

use fabro_llm::client::Client;
use fabro_llm::generate::{self, GenerateParams};
use fabro_llm::types::TimeoutOptions;
use fabro_model::ProviderId;
use fabro_types::{Graph, MAX_RUN_TITLE_CHARS, RunId};
use serde::Serialize;
use toml::Value as TomlValue;

const TRUNCATED_MARKER: &str = "...[truncated]";
const MAX_PROMPT_SECTION_CHARS: usize = 4_000;

pub(crate) struct TitlePromptInput<'a> {
    pub(crate) run_id:          &'a RunId,
    pub(crate) current_title:   &'a str,
    pub(crate) workflow_target: Option<&'a str>,
    pub(crate) run_inputs:      &'a HashMap<String, TomlValue>,
    pub(crate) workflow:        &'a WorkflowSummary,
}

pub(crate) struct GenerateTitleInput<'a> {
    pub(crate) client:      Arc<Client>,
    pub(crate) model_id:    String,
    pub(crate) provider_id: ProviderId,
    pub(crate) prompt:      TitlePromptInput<'a>,
}

pub(crate) async fn generate_title_or_current(input: GenerateTitleInput<'_>) -> String {
    let current_title = input.prompt.current_title.to_string();
    let prompt = build_title_prompt(&input.prompt);
    let params = GenerateParams::new(input.model_id, input.client)
        .provider(input.provider_id.to_string())
        .prompt(prompt)
        .max_tokens(64)
        .max_retries(0)
        .timeout(TimeoutOptions {
            total:    Some(10.0),
            per_step: Some(5.0),
        });

    let result = match generate::generate_object(params, title_response_schema()).await {
        Ok(result) => result,
        Err(err) => {
            tracing::debug!(error = %err, "Run title generation failed");
            return current_title;
        }
    };
    result
        .output
        .as_ref()
        .and_then(|output| output.get("title"))
        .and_then(serde_json::Value::as_str)
        .and_then(normalize_generated_title)
        .unwrap_or(current_title)
}

fn build_title_prompt(input: &TitlePromptInput<'_>) -> String {
    let workflow_identity = serde_json::json!({
        "run_id": input.run_id.to_string(),
        "current_deterministic_title": input.current_title,
        "target": input.workflow_target,
        "name": input.workflow.graph_name,
        "goal": input.workflow.goal,
    });
    let identity = pretty_json(&workflow_identity);
    let inputs = pretty_json(input.run_inputs);
    let workflow = pretty_json(input.workflow);

    format!(
        r#"Generate a concise, human-readable title for this Fabro workflow run.

Base the title on the workflow identity, workflow goal, and run input values.
Preserve meaningful proper nouns, ticket IDs, repositories, branches, environments, and explicit user goals.
Return only structured JSON with one field: {{"title":"..."}}.
The title must be a single line, not blank, and no more than {MAX_RUN_TITLE_CHARS} characters.

Workflow identity:
```json
{}
```

Run inputs (raw values, not redacted):
```json
{}
```

Workflow summary:
```json
{}
```
"#,
        truncate_section(&identity, MAX_PROMPT_SECTION_CHARS),
        truncate_section(&inputs, MAX_PROMPT_SECTION_CHARS),
        truncate_section(&workflow, MAX_PROMPT_SECTION_CHARS),
    )
}

fn normalize_generated_title(title: &str) -> Option<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return None;
    }
    Some(trimmed.chars().take(MAX_RUN_TITLE_CHARS).collect())
}

fn title_response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["title"],
        "properties": {
            "title": {
                "type": "string",
                "description": "Concise single-line run title"
            }
        }
    })
}

#[derive(Serialize)]
pub(crate) struct WorkflowSummary {
    pub(crate) graph_name:  String,
    pub(crate) goal:        String,
    pub(crate) stage_count: usize,
    pub(crate) edge_count:  usize,
    pub(crate) stages:      Vec<StageSummary>,
}

#[derive(Serialize)]
pub(crate) struct StageSummary {
    id:           String,
    label:        String,
    handler_type: Option<String>,
}

pub(crate) fn workflow_summary(graph: &Graph) -> WorkflowSummary {
    let mut stages = graph
        .nodes
        .values()
        .map(|node| StageSummary {
            id:           node.id.clone(),
            label:        node.label().to_string(),
            handler_type: node.handler_type().map(str::to_string),
        })
        .collect::<Vec<_>>();
    stages.sort_by(|left, right| left.id.cmp(&right.id));

    WorkflowSummary {
        graph_name: graph.name.clone(),
        goal: graph.goal().to_string(),
        stage_count: stages.len(),
        edge_count: graph.edges.len(),
        stages,
    }
}

fn pretty_json(value: &impl Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string())
}

fn truncate_section(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let keep = max_chars.saturating_sub(TRUNCATED_MARKER.chars().count());
    let truncated = value.chars().take(keep).collect::<String>();
    format!("{truncated}{TRUNCATED_MARKER}")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use fabro_graphviz::parser;
    use fabro_llm::client::Client;
    use fabro_llm::error::Error as LlmError;
    use fabro_llm::provider::{ProviderAdapter, StreamEventStream};
    use fabro_llm::token_count::InputTokenCount;
    use fabro_llm::types::{FinishReason, Message, Request, Response, StreamEvent, TokenCounts};
    use fabro_model::ProviderId;
    use fabro_types::RunId;
    use futures_util::stream;
    use toml::Value as TomlValue;

    use super::*;

    fn title_test_graph() -> fabro_types::Graph {
        parser::parse(
            r#"digraph Ship {
                graph [goal="Deploy API token SECRET_123 to production"]
                start [shape=Mdiamond, label="Start"]
                plan [shape=box, label="Plan rollout"]
                deploy [shape=parallelogram, label="Deploy"]
                exit [shape=Msquare, label="Exit"]
                start -> plan -> deploy -> exit
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn prompt_includes_goal_inputs_and_workflow_summary_without_redaction() {
        let run_id = RunId::new();
        let graph = title_test_graph();
        let summary = workflow_summary(&graph);
        let inputs = HashMap::from([
            (
                "api_key".to_string(),
                TomlValue::String("SECRET_123".to_string()),
            ),
            (
                "region".to_string(),
                TomlValue::String("us-east-1".to_string()),
            ),
        ]);

        let prompt = build_title_prompt(&TitlePromptInput {
            run_id:          &run_id,
            current_title:   "Deploy API token SECRET_123 to production",
            workflow_target: Some("workflows/deploy.fabro"),
            run_inputs:      &inputs,
            workflow:        &summary,
        });

        assert!(prompt.contains("Deploy API token SECRET_123 to production"));
        assert!(prompt.contains("\"api_key\": \"SECRET_123\""));
        assert!(prompt.contains("workflows/deploy.fabro"));
        assert!(prompt.contains("\"id\": \"deploy\""));
        assert!(prompt.contains("\"handler_type\": \"command\""));
        assert!(!prompt.contains("[REDACTED]"));
    }

    #[test]
    fn prompt_bounds_large_input_and_workflow_sections() {
        let run_id = RunId::new();
        let graph = title_test_graph();
        let summary = workflow_summary(&graph);
        let inputs = HashMap::from([(
            "large".to_string(),
            TomlValue::String("x".repeat(MAX_PROMPT_SECTION_CHARS * 2)),
        )]);

        let prompt = build_title_prompt(&TitlePromptInput {
            run_id:          &run_id,
            current_title:   "Current",
            workflow_target: Some("workflow.fabro"),
            run_inputs:      &inputs,
            workflow:        &summary,
        });

        // Section truncation: per-section budget × 3 + small boilerplate.
        assert!(prompt.chars().count() < MAX_PROMPT_SECTION_CHARS * 3 + 1_000);
        assert!(prompt.contains("...[truncated]"));
        // One section was truncated, the others were not; ensure we don't
        // emit a doubly-truncated marker run.
        assert!(!prompt.contains("...[truncated]...[truncated]"));
    }

    #[test]
    fn generated_title_normalization_accepts_and_truncates_valid_titles() {
        let normalized = normalize_generated_title("  Ship the API rollout  ").unwrap();
        assert_eq!(normalized, "Ship the API rollout");

        let long = normalize_generated_title(&"a".repeat(101)).unwrap();
        assert_eq!(long.chars().count(), 100);
    }

    #[test]
    fn generated_title_normalization_rejects_blank_and_control_output() {
        assert!(normalize_generated_title("   ").is_none());
        assert!(normalize_generated_title("first\nsecond").is_none());
        assert!(normalize_generated_title("first\tsecond").is_none());
    }

    #[tokio::test]
    async fn generation_uses_supplied_small_default_model_id() {
        let (title, captured) = title_with_mocked_response(r#"{"title":"Generated title"}"#).await;

        assert_eq!(title, "Generated title");
        let captured = captured.lock().unwrap();
        assert_eq!(captured[0].model, "small-model");
        assert_eq!(captured[0].provider.as_deref(), Some("openai"));
        assert_eq!(captured[0].max_tokens, Some(64));
    }

    #[tokio::test]
    async fn invalid_or_failed_generation_returns_current_title() {
        let (invalid, _) = title_with_mocked_response(r#"{"title":"first\nsecond"}"#).await;
        assert_eq!(invalid, "Current");

        let (invalid_shape, _) = title_with_mocked_response(r#"{"name":"missing"}"#).await;
        assert_eq!(invalid_shape, "Current");
    }

    async fn title_with_mocked_response(response_text: &str) -> (String, Arc<Mutex<Vec<Request>>>) {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let provider = Arc::new(CapturingProvider {
            captured:      Arc::clone(&captured),
            response_text: response_text.to_string(),
        });
        let client = Arc::new(Client::new(
            HashMap::from([("openai".to_string(), provider as Arc<dyn ProviderAdapter>)]),
            Some("openai".to_string()),
            Vec::new(),
        ));
        let run_id = RunId::new();
        let graph = title_test_graph();
        let summary = workflow_summary(&graph);
        let inputs = HashMap::new();
        let title = generate_title_or_current(GenerateTitleInput {
            client,
            model_id: "small-model".to_string(),
            provider_id: ProviderId::openai(),
            prompt: TitlePromptInput {
                run_id:          &run_id,
                current_title:   "Current",
                workflow_target: Some("workflow.fabro"),
                run_inputs:      &inputs,
                workflow:        &summary,
            },
        })
        .await;
        (title, captured)
    }

    struct CapturingProvider {
        captured:      Arc<Mutex<Vec<Request>>>,
        response_text: String,
    }

    #[async_trait]
    impl ProviderAdapter for CapturingProvider {
        #[expect(
            clippy::unnecessary_literal_bound,
            reason = "ProviderAdapter trait signature returns &str."
        )]
        fn name(&self) -> &str {
            "openai"
        }

        async fn complete(&self, request: &Request) -> Result<Response, LlmError> {
            self.captured.lock().unwrap().push(request.clone());
            Ok(Response {
                id:            "resp_title".to_string(),
                model:         request.model.clone(),
                provider:      "openai".to_string(),
                message:       Message::assistant(self.response_text.clone()),
                finish_reason: FinishReason::Stop,
                usage:         TokenCounts::default(),
                raw:           None,
                warnings:      Vec::new(),
                rate_limit:    None,
                cost_usd:      None,
                cost_source:   None,
            })
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, LlmError> {
            Ok(Pin::from(Box::new(stream::empty::<
                Result<StreamEvent, LlmError>,
            >())))
        }

        async fn count_input_tokens(
            &self,
            _request: &Request,
        ) -> Result<Option<InputTokenCount>, LlmError> {
            Ok(None)
        }
    }
}
