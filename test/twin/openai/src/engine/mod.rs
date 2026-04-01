pub mod defaults;
pub mod failures;
pub mod plan;
pub mod scenario;

use crate::{
    openai::models::{ChatCompletionsRequest, OpenAiError, ResponsesRequest, ToolChoiceMode},
    state::{AppState, NamespaceKey},
};

use self::defaults::{build_default_chat_plan, build_default_response_plan};
use self::failures::{ExecutionOutcome, SuccessOutcome, TransportOptions};
use self::plan::ResponsePlan;
use self::scenario::RequestContext;

pub fn execute_responses_request(
    state: &AppState,
    namespace: &NamespaceKey,
    request: &ResponsesRequest,
) -> Result<ExecutionOutcome, OpenAiError> {
    request.validate()?;
    let context = RequestContext {
        endpoint: "responses".to_owned(),
        model: request.model.clone(),
        stream: request.stream,
        metadata: request.metadata.clone(),
        input_text: request.extract_user_text(),
    };
    state.log_request(namespace, context.clone());

    if let Some(scenario) = state.take_matching_scenario(namespace, &context) {
        return match scenario.execute_for_responses(state.next_response_id(namespace), request) {
            ExecutionOutcome::Success(success) => Ok(ExecutionOutcome::Success(
                enforce_tool_choice(request.tool_choice_mode(), success)?,
            )),
            outcome => Ok(outcome),
        };
    }

    Ok(ExecutionOutcome::Success(enforce_tool_choice(
        request.tool_choice_mode(),
        SuccessOutcome {
            plan: build_default_response_plan(state.next_response_id(namespace), request),
            transport: TransportOptions::default(),
        },
    )?))
}

pub fn execute_chat_request(
    state: &AppState,
    namespace: &NamespaceKey,
    request: &ChatCompletionsRequest,
) -> Result<ExecutionOutcome, OpenAiError> {
    request.validate()?;
    let context = RequestContext {
        endpoint: "chat.completions".to_owned(),
        model: request.model.clone(),
        stream: request.stream,
        metadata: serde_json::Map::new(),
        input_text: request.extract_user_text(),
    };
    state.log_request(namespace, context.clone());

    if let Some(scenario) = state.take_matching_scenario(namespace, &context) {
        return match scenario.execute_for_chat(state.next_response_id(namespace), request) {
            ExecutionOutcome::Success(success) => Ok(ExecutionOutcome::Success(
                enforce_tool_choice(request.tool_choice_mode(), success)?,
            )),
            outcome => Ok(outcome),
        };
    }

    Ok(ExecutionOutcome::Success(enforce_tool_choice(
        request.tool_choice_mode(),
        SuccessOutcome {
            plan: build_default_chat_plan(
                state.next_response_id(namespace),
                request.model.clone(),
                &request.extract_user_text(),
                request.response_format(),
                request.reasoning_requested(),
            ),
            transport: TransportOptions::default(),
        },
    )?))
}

fn enforce_tool_choice(
    tool_choice: Option<ToolChoiceMode>,
    success: SuccessOutcome,
) -> Result<SuccessOutcome, OpenAiError> {
    validate_tool_choice_against_plan(tool_choice, &success.plan)?;
    Ok(success)
}

fn validate_tool_choice_against_plan(
    tool_choice: Option<ToolChoiceMode>,
    plan: &ResponsePlan,
) -> Result<(), OpenAiError> {
    match tool_choice {
        None | Some(ToolChoiceMode::Auto) => Ok(()),
        Some(ToolChoiceMode::NoTool) if plan.tool_calls.is_empty() => Ok(()),
        Some(ToolChoiceMode::NoTool) => Err(OpenAiError::invalid_request(
            "tool_choice",
            "tool_choice forbids tool calls for this request",
        )),
        Some(ToolChoiceMode::Required) if !plan.tool_calls.is_empty() => Ok(()),
        Some(ToolChoiceMode::Required) => Err(OpenAiError::invalid_request(
            "tool_choice",
            "tool_choice required a tool call but none was planned",
        )),
        Some(ToolChoiceMode::Function(name))
            if plan
                .tool_calls
                .iter()
                .any(|tool_call| tool_call.name == name) =>
        {
            Ok(())
        }
        Some(ToolChoiceMode::Function(name)) => Err(OpenAiError::invalid_request(
            "tool_choice",
            &format!("tool_choice requested function `{name}` but it was not planned"),
        )),
    }
}
