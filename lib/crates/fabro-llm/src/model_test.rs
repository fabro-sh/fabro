use std::sync::Arc;
use std::time::Duration;

pub use fabro_model::ModelTestMode;
use fabro_model::{Model, Provider};
use strum::IntoStaticStr;
use tokio::time;

use crate::client::Client;
use crate::generate::{self, GenerateParams};
use crate::tools::Tool;
use crate::types::{GenerateResult, ReasoningEffort};

#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum ModelTestStatus {
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelTestOutcome {
    pub status:        ModelTestStatus,
    pub error_message: Option<String>,
}

impl ModelTestOutcome {
    #[must_use]
    pub fn ok() -> Self {
        Self {
            status:        ModelTestStatus::Ok,
            error_message: None,
        }
    }

    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            status:        ModelTestStatus::Error,
            error_message: Some(message.into()),
        }
    }
}

pub async fn run_model_test(
    info: &Model,
    mode: ModelTestMode,
    client: Arc<Client>,
) -> ModelTestOutcome {
    match mode {
        ModelTestMode::Basic => run_basic_test(info, client).await,
        ModelTestMode::Deep => run_deep_test(info, client).await,
    }
}

async fn run_basic_test(info: &Model, client: Arc<Client>) -> ModelTestOutcome {
    run_basic_model_probe(&info.id, info.provider, client).await
}

/// Run the cheap single-prompt model availability probe without requiring a
/// catalog-backed [`Model`].
pub async fn run_basic_model_probe(
    model_id: &str,
    provider: Provider,
    client: Arc<Client>,
) -> ModelTestOutcome {
    let params = GenerateParams::new(model_id, client)
        .provider(<&'static str>::from(provider))
        .prompt("Say OK")
        .max_tokens(16);

    let result = time::timeout(
        Duration::from_secs(ModelTestMode::Basic.timeout_secs()),
        generate::generate(params),
    )
    .await;

    match result {
        Ok(Ok(_)) => ModelTestOutcome::ok(),
        Ok(Err(err)) => ModelTestOutcome::error(err.to_string()),
        Err(_) => ModelTestOutcome::error("timeout (30s)"),
    }
}

async fn run_deep_test(info: &Model, client: Arc<Client>) -> ModelTestOutcome {
    let Some(params) = build_deep_test_params(info, client) else {
        return ModelTestOutcome::error("model does not support tools");
    };

    let result = time::timeout(
        Duration::from_secs(ModelTestMode::Deep.timeout_secs()),
        generate::generate(params),
    )
    .await;

    match result {
        Ok(Ok(gen_result)) => match validate_deep_result(&gen_result) {
            Ok(()) => ModelTestOutcome::ok(),
            Err(message) => ModelTestOutcome::error(message),
        },
        Ok(Err(err)) => ModelTestOutcome::error(err.to_string()),
        Err(_) => ModelTestOutcome::error("timeout (90s)"),
    }
}

fn build_deep_test_params(info: &Model, client: Arc<Client>) -> Option<GenerateParams> {
    if !info.features.tools {
        return None;
    }

    let add_tool = Tool::active(
        "add",
        "Add two integers and return the sum",
        serde_json::json!({
            "type": "object",
            "properties": {
                "a": { "type": "integer", "description": "First number" },
                "b": { "type": "integer", "description": "Second number" }
            },
            "required": ["a", "b"]
        }),
        |args, _ctx| async move {
            let a = args
                .get("a")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let b = args
                .get("b")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            Ok(serde_json::json!(a + b))
        },
    );

    let mut params = GenerateParams::new(&info.id, client)
        .provider(<&'static str>::from(info.provider))
        .prompt(
            "Use the add tool twice: first add 15 and 27, then add that result to 42. \
             Finally, tell me whether the grand total is even or odd and why.",
        )
        .tools(vec![add_tool])
        .max_tool_rounds(5)
        .max_tokens(1024);

    if info.features.reasoning {
        params = params.reasoning_effort(ReasoningEffort::High);
    }

    Some(params)
}

fn validate_deep_result(result: &GenerateResult) -> Result<(), String> {
    if result.steps.len() < 2 {
        return Err("model did not call tool".to_string());
    }

    if result.steps[0].tool_results.is_empty() {
        return Err("tool was not executed".to_string());
    }

    if !result.response.text().contains("84") {
        return Err("wrong answer".to_string());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use fabro_model::{ModelCosts, ModelFeatures, ModelLimits, Provider};

    use super::*;
    use crate::types::{FinishReason, Message, Response, StepResult, TokenCounts, ToolResult};

    fn test_model_with(features: ModelFeatures) -> Model {
        Model {
            id: "test-model".to_string(),
            provider: Provider::Anthropic,
            family: "test".to_string(),
            display_name: "Test Model".to_string(),
            limits: ModelLimits {
                context_window: 200_000,
                max_output:     Some(8_000),
            },
            training: None,
            knowledge_cutoff: None,
            features,
            costs: ModelCosts {
                input_cost_per_mtok:       None,
                output_cost_per_mtok:      None,
                cache_input_cost_per_mtok: None,
            },
            estimated_output_tps: None,
            aliases: vec![],
            default: false,
            configured: false,
        }
    }

    fn response_with_text(text: &str) -> Response {
        Response {
            id:            "resp_1".to_string(),
            model:         "test-model".to_string(),
            provider:      "anthropic".to_string(),
            message:       Message::assistant(text),
            finish_reason: FinishReason::Stop,
            usage:         TokenCounts::default(),
            raw:           None,
            warnings:      vec![],
            rate_limit:    None,
        }
    }

    fn empty_test_client() -> Arc<Client> {
        Arc::new(Client::new(HashMap::new(), None, vec![]))
    }

    #[tokio::test]
    async fn run_model_test_deep_errors_when_model_lacks_tools() {
        let info = test_model_with(ModelFeatures {
            tools:     false,
            vision:    false,
            reasoning: true,
            effort:    true,
        });

        let outcome = run_model_test(&info, ModelTestMode::Deep, empty_test_client()).await;

        assert_eq!(outcome.status, ModelTestStatus::Error);
        assert_eq!(
            outcome.error_message.as_deref(),
            Some("model does not support tools")
        );
    }

    #[test]
    fn validate_deep_result_does_not_fail_only_for_missing_reasoning() {
        let tool_results = vec![ToolResult::success("call_1", serde_json::json!(42))];
        let first_step = StepResult {
            response:     response_with_text("tool step"),
            tool_results: tool_results.clone(),
        };
        let second_step = StepResult {
            response:     response_with_text("84 is even"),
            tool_results: vec![],
        };
        let result = GenerateResult {
            response: response_with_text("84 is even"),
            tool_results,
            total_usage: TokenCounts::default(),
            steps: vec![first_step, second_step],
            output: None,
        };

        assert_eq!(validate_deep_result(&result), Ok(()));
    }
}
