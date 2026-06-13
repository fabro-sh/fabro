use std::sync::Arc;

use super::super::{
    ApiError, AppState, CompletionResponse, CompletionToolChoiceMode, CompletionUsage,
    CreateCompletionRequest, FinishReason, GenerateParams, IntoResponse, Json, LlmMessage,
    LlmRequest, RequiredUser, Response, Router, State, StatusCode, ToolChoice, ToolDefinition,
    Ulid, error, generate_object, info, post, warn,
};
use super::llm_sse;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/completions", post(create_completion))
}

fn finish_reason_to_api_stop_reason(reason: &FinishReason) -> String {
    match reason {
        FinishReason::Stop => "end_turn".to_string(),
        FinishReason::Length => "max_tokens".to_string(),
        FinishReason::ToolCalls => "tool_calls".to_string(),
        FinishReason::ContentFilter => "content_filter".to_string(),
        FinishReason::Error => "error".to_string(),
        FinishReason::Other(s) => s.clone(),
    }
}

async fn create_completion(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateCompletionRequest>,
) -> Response {
    // Resolve model
    let catalog = state.catalog();
    let model_id = req
        .model
        .unwrap_or_else(|| catalog.default_model().id.clone());

    let catalog_info = catalog.get(&model_id);

    // Resolve provider: explicit request > catalog > None
    let explicit_provider = req.provider;
    let provider_name = explicit_provider
        .clone()
        .or_else(|| catalog_info.map(|i| i.provider.to_string()));

    info!(model = %model_id, provider = ?provider_name, "Completion request received");

    // Build messages list. Request messages are already the canonical
    // `fabro_types::Message` — the API schema reuses it via build.rs
    // `with_replacement`, so no conversion is needed.
    let mut messages: Vec<LlmMessage> = Vec::new();
    if let Some(system) = req.system {
        messages.push(LlmMessage::system(system));
    }
    messages.extend(req.messages);

    // Convert tools
    let tools: Option<Vec<ToolDefinition>> = if req.tools.is_empty() {
        None
    } else {
        Some(
            req.tools
                .into_iter()
                .map(|t| ToolDefinition {
                    name:        t.name,
                    description: t.description,
                    parameters:  t.parameters,
                })
                .collect(),
        )
    };

    // Convert tool_choice
    let tool_choice: Option<ToolChoice> = req.tool_choice.map(|tc| match tc.mode {
        CompletionToolChoiceMode::Auto => ToolChoice::Auto,
        CompletionToolChoiceMode::None => ToolChoice::None,
        CompletionToolChoiceMode::Required => ToolChoice::Required,
        CompletionToolChoiceMode::Named => ToolChoice::named(tc.tool_name.unwrap_or_default()),
    });

    // Build the LLM request
    let request = LlmRequest {
        model: model_id.clone(),
        messages,
        provider: provider_name.clone(),
        tools,
        tool_choice,
        response_format: None,
        temperature: req.temperature,
        top_p: req.top_p,
        max_tokens: req.max_tokens,
        stop_sequences: if req.stop_sequences.is_empty() {
            None
        } else {
            Some(req.stop_sequences)
        },
        reasoning_effort: req.reasoning_effort.as_deref().and_then(|s| s.parse().ok()),
        speed: None,
        metadata: None,
        provider_options: req.provider_options,
    };

    // Force non-streaming for structured output
    let use_stream = req.stream && req.schema.is_none();

    let llm_result = match state.resolve_llm_client().await {
        Ok(result) => result,
        Err(err) => {
            error!(error = ?err, "Failed to create LLM client");
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create LLM client: {err}"),
            )
            .into_response();
        }
    };
    for (provider, issue) in &llm_result.auth_issues {
        warn!(provider = %provider, error = %issue, "LLM provider unavailable due to auth issue");
    }
    for issue in &llm_result.registration_issues {
        warn!(provider = %issue.provider, error = %issue.error, "LLM provider unavailable due to registration issue");
    }
    let client = llm_result.client;
    if let Some(provider) = explicit_provider.as_deref() {
        if !client.has_provider(provider) {
            return ApiError::bad_request(format!("Provider \"{provider}\" is not configured"))
                .into_response();
        }
    }

    if use_stream {
        // Streaming path: forward all StreamEvents as SSE
        let stream_result = match client.stream(&request).await {
            Ok(s) => s,
            Err(e) => {
                return ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                    .into_response();
            }
        };

        llm_sse::stream_response(stream_result, state.shutdown_token())
    } else {
        // Non-streaming path
        let msg_id = Ulid::new().to_string();

        if let Some(schema) = req.schema {
            // Structured output uses generate_object for JSON parsing logic
            let mut params =
                GenerateParams::new(&request.model, std::sync::Arc::new(client.clone()))
                    .messages(request.messages);
            if let Some(ref p) = request.provider {
                params = params.provider(p);
            }
            if let Some(temp) = request.temperature {
                params = params.temperature(temp);
            }
            if let Some(max_tokens) = request.max_tokens {
                params = params.max_tokens(max_tokens);
            }
            if let Some(top_p) = request.top_p {
                params = params.top_p(top_p);
            }
            match generate_object(params, schema).await {
                Ok(result) => {
                    // `result.finish_reason` / `result.usage` resolve through
                    // GenerateResult's Deref to the inner Response; move the
                    // Response out once so `message` can be taken by value.
                    let output = result.output;
                    let response = result.response;
                    let stop_reason = finish_reason_to_api_stop_reason(&response.finish_reason);
                    Json(CompletionResponse {
                        id: msg_id,
                        model: model_id,
                        message: response.message,
                        stop_reason,
                        usage: CompletionUsage {
                            input_tokens:  response.usage.input_tokens,
                            output_tokens: response.usage.output_tokens,
                        },
                        output,
                        cost_usd: response.cost_usd,
                        cost_source: response.cost_source,
                    })
                    .into_response()
                }
                Err(e) => ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                    .into_response(),
            }
        } else {
            match client.complete(&request).await {
                Ok(response) => {
                    let stop_reason = finish_reason_to_api_stop_reason(&response.finish_reason);
                    Json(CompletionResponse {
                        id: response.id,
                        model: response.model,
                        message: response.message,
                        stop_reason,
                        usage: CompletionUsage {
                            input_tokens:  response.usage.input_tokens,
                            output_tokens: response.usage.output_tokens,
                        },
                        output: None,
                        cost_usd: response.cost_usd,
                        cost_source: response.cost_source,
                    })
                    .into_response()
                }
                Err(e) => ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                    .into_response(),
            }
        }
    }
}
