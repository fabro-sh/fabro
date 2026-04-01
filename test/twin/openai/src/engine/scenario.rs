use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::openai::models::{ChatCompletionsRequest, ResponsesRequest};

use super::{
    failures::{ErrorOutcome, ExecutionOutcome, SuccessOutcome, TransportOptions},
    plan::{ResponsePlan, ToolCallPlan},
};

#[derive(Clone, Debug, Deserialize)]
pub struct ScenarioEnvelope {
    pub scenarios: Vec<Scenario>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Scenario {
    pub matcher: ScenarioMatcher,
    pub script: ScenarioScript,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ScenarioMatcher {
    pub endpoint: String,
    pub model: Option<String>,
    pub stream: Option<bool>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    pub input_contains: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScenarioScript {
    Success {
        response_text: Option<String>,
        reasoning: Option<Vec<String>>,
        structured_output: Option<Value>,
        tool_calls: Option<Vec<ToolCallTemplate>>,
        delay_before_headers_ms: Option<u64>,
        inter_event_delay_ms: Option<u64>,
        close_after_chunks: Option<usize>,
        malformed_sse: Option<bool>,
    },
    Error {
        status: u16,
        message: String,
        error_type: String,
        code: String,
        retry_after: Option<String>,
        delay_before_headers_ms: Option<u64>,
    },
    Hang {
        delay_before_headers_ms: Option<u64>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolCallTemplate {
    pub id: Option<String>,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug)]
pub struct RequestContext {
    pub endpoint: String,
    pub model: String,
    pub stream: bool,
    pub metadata: Map<String, Value>,
    pub input_text: String,
}

impl ScenarioScript {
    pub fn script_kind(&self) -> &str {
        match self {
            Self::Success { .. } => "success",
            Self::Error { .. } => "error",
            Self::Hang { .. } => "hang",
        }
    }
}

impl Scenario {
    pub fn matches(&self, request: &RequestContext) -> bool {
        if self.matcher.endpoint != request.endpoint {
            return false;
        }

        if let Some(model) = &self.matcher.model {
            if model != &request.model {
                return false;
            }
        }

        if let Some(stream) = self.matcher.stream {
            if stream != request.stream {
                return false;
            }
        }

        if let Some(needle) = &self.matcher.input_contains {
            if !request.input_text.contains(needle) {
                return false;
            }
        }

        self.matcher.metadata.iter().all(|(key, value)| {
            request
                .metadata
                .get(key)
                .is_some_and(|candidate| candidate == value)
        })
    }

    pub fn execute_for_responses(
        &self,
        response_number: u64,
        request: &ResponsesRequest,
    ) -> ExecutionOutcome {
        match &self.script {
            ScenarioScript::Success {
                response_text,
                reasoning,
                structured_output,
                tool_calls,
                delay_before_headers_ms,
                inter_event_delay_ms,
                close_after_chunks,
                malformed_sse,
            } => ExecutionOutcome::Success(SuccessOutcome {
                plan: build_plan_from_script(
                    response_number,
                    request.model.clone(),
                    &request.extract_user_text(),
                    response_text.clone(),
                    reasoning.clone().unwrap_or_default(),
                    structured_output.clone(),
                    tool_calls.clone().unwrap_or_default(),
                ),
                transport: TransportOptions {
                    delay_before_headers_ms: delay_before_headers_ms.unwrap_or_default(),
                    inter_event_delay_ms: inter_event_delay_ms.unwrap_or_default(),
                    close_after_chunks: *close_after_chunks,
                    malformed_sse: malformed_sse.unwrap_or(false),
                },
            }),
            ScenarioScript::Error {
                status,
                message,
                error_type,
                code,
                retry_after,
                delay_before_headers_ms,
            } => ExecutionOutcome::Error(ErrorOutcome::new(
                StatusCode::from_u16(*status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                message.clone(),
                error_type.clone(),
                code.clone(),
                retry_after.clone(),
                delay_before_headers_ms.unwrap_or_default(),
            )),
            ScenarioScript::Hang {
                delay_before_headers_ms,
            } => ExecutionOutcome::Hang {
                delay_before_headers_ms: delay_before_headers_ms.unwrap_or_default(),
            },
        }
    }

    pub fn execute_for_chat(
        &self,
        response_number: u64,
        request: &ChatCompletionsRequest,
    ) -> ExecutionOutcome {
        match &self.script {
            ScenarioScript::Success {
                response_text,
                reasoning,
                structured_output,
                tool_calls,
                delay_before_headers_ms,
                inter_event_delay_ms,
                close_after_chunks,
                malformed_sse,
            } => ExecutionOutcome::Success(SuccessOutcome {
                plan: build_plan_from_script(
                    response_number,
                    request.model.clone(),
                    &request.extract_user_text(),
                    response_text.clone(),
                    reasoning.clone().unwrap_or_default(),
                    structured_output.clone(),
                    tool_calls.clone().unwrap_or_default(),
                ),
                transport: TransportOptions {
                    delay_before_headers_ms: delay_before_headers_ms.unwrap_or_default(),
                    inter_event_delay_ms: inter_event_delay_ms.unwrap_or_default(),
                    close_after_chunks: *close_after_chunks,
                    malformed_sse: malformed_sse.unwrap_or(false),
                },
            }),
            ScenarioScript::Error {
                status,
                message,
                error_type,
                code,
                retry_after,
                delay_before_headers_ms,
            } => ExecutionOutcome::Error(ErrorOutcome::new(
                StatusCode::from_u16(*status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                message.clone(),
                error_type.clone(),
                code.clone(),
                retry_after.clone(),
                delay_before_headers_ms.unwrap_or_default(),
            )),
            ScenarioScript::Hang {
                delay_before_headers_ms,
            } => ExecutionOutcome::Hang {
                delay_before_headers_ms: delay_before_headers_ms.unwrap_or_default(),
            },
        }
    }
}

fn build_plan_from_script(
    response_number: u64,
    model: String,
    default_input: &str,
    response_text: Option<String>,
    reasoning: Vec<String>,
    structured_output: Option<Value>,
    tool_calls: Vec<ToolCallTemplate>,
) -> ResponsePlan {
    let output_text = match response_text {
        Some(response_text) => response_text,
        None if tool_calls.is_empty() && structured_output.is_none() => {
            format!("deterministic: {default_input}")
        }
        None => String::new(),
    };
    ResponsePlan {
        id: format!("resp_{response_number:06}"),
        created: response_number,
        model,
        response_text: output_text,
        structured_output,
        reasoning,
        tool_calls: tool_calls
            .into_iter()
            .enumerate()
            .map(|(index, tool_call)| ToolCallPlan {
                id: tool_call
                    .id
                    .unwrap_or_else(|| format!("call_{response_number}_{index}")),
                name: tool_call.name,
                arguments: tool_call.arguments,
            })
            .collect(),
        input_tokens: 1,
        output_tokens: 5,
    }
}
