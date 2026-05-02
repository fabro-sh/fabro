use std::sync::Arc;

use super::super::{
    ApiError, AppState, CompletionContentPart, CompletionMessage, CompletionMessageRole,
    CompletionResponse, CompletionToolChoiceMode, CompletionUsage, ContentPart,
    CreateCompletionRequest, Duration, Event, FinishReason, GenerateParams, IntoResponse, Json,
    KeepAlive, LlmMessage, LlmRequest, RequiredUser, Response, Role, Router, Sse, State,
    StatusCode, ToolChoice, ToolDefinition, Ulid, error, generate_object, info, post, warn,
};

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

fn convert_api_message(msg: &CompletionMessage) -> LlmMessage {
    let role = match msg.role {
        CompletionMessageRole::System => Role::System,
        CompletionMessageRole::User => Role::User,
        CompletionMessageRole::Assistant => Role::Assistant,
        CompletionMessageRole::Tool => Role::Tool,
        CompletionMessageRole::Developer => Role::Developer,
    };
    let content: Vec<ContentPart> = msg
        .content
        .iter()
        .filter_map(|part| {
            let json = serde_json::to_value(part).ok()?;
            serde_json::from_value(json).ok()
        })
        .collect();
    LlmMessage {
        role,
        content,
        name: msg.name.clone(),
        tool_call_id: msg.tool_call_id.clone(),
    }
}

fn convert_llm_message(msg: &LlmMessage) -> CompletionMessage {
    let role = match msg.role {
        Role::System => CompletionMessageRole::System,
        Role::User => CompletionMessageRole::User,
        Role::Assistant => CompletionMessageRole::Assistant,
        Role::Tool => CompletionMessageRole::Tool,
        Role::Developer => CompletionMessageRole::Developer,
    };
    let content: Vec<CompletionContentPart> = msg
        .content
        .iter()
        .filter_map(|part| {
            let json = serde_json::to_value(part).ok()?;
            serde_json::from_value(json).ok()
        })
        .collect();
    CompletionMessage {
        role,
        content,
        name: msg.name.clone(),
        tool_call_id: msg.tool_call_id.clone(),
    }
}

async fn create_completion(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateCompletionRequest>,
) -> Response {
    // Resolve model
    let model_id = req.model.unwrap_or_else(|| {
        fabro_model::Catalog::builtin()
            .list(None)
            .first()
            .map_or_else(|| "claude-sonnet-4-5".to_string(), |m| m.id.clone())
    });

    let catalog_info = fabro_model::Catalog::builtin().get(&model_id);

    // Resolve provider: explicit request > catalog > None
    let provider_name = req
        .provider
        .or_else(|| catalog_info.map(|i| i.provider.to_string()));

    info!(model = %model_id, provider = ?provider_name, "Completion request received");

    // Build messages list
    let mut messages: Vec<LlmMessage> = Vec::new();
    if let Some(system) = req.system {
        messages.push(LlmMessage::system(system));
    }
    for msg in &req.messages {
        messages.push(convert_api_message(msg));
    }

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
        provider: provider_name,
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
    let client = llm_result.client;

    if use_stream {
        // Streaming path: forward all StreamEvents as SSE
        let stream_result = match client.stream(&request).await {
            Ok(s) => s,
            Err(e) => {
                return ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                    .into_response();
            }
        };

        let sse_stream = tokio_stream::StreamExt::filter_map(stream_result, |event| match event {
            Ok(ref evt) => match serde_json::to_string(evt) {
                Ok(json) => Some(Ok::<_, std::convert::Infallible>(
                    Event::default().event("stream_event").data(json),
                )),
                Err(e) => Some(Ok(Event::default().event("stream_event").data(
                    serde_json::json!({
                        "type": "error",
                        "error": {"Stream": {"message": format!("failed to serialize event: {e}")}},
                        "raw": null
                    })
                    .to_string(),
                ))),
            },
            Err(e) => Some(Ok(Event::default().event("stream_event").data(
                serde_json::json!({
                    "type": "error",
                    "error": {"Stream": {"message": e.to_string()}},
                    "raw": null
                })
                .to_string(),
            ))),
        });

        Sse::new(sse_stream)
            .keep_alive(
                KeepAlive::new().interval(Duration::from_secs(15)).event(
                    Event::default()
                        .event("ping")
                        .data(serde_json::json!({"type": "ping"}).to_string()),
                ),
            )
            .into_response()
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
                Ok(result) => Json(CompletionResponse {
                    id:          msg_id,
                    model:       model_id,
                    message:     convert_llm_message(&result.response.message),
                    stop_reason: finish_reason_to_api_stop_reason(&result.finish_reason),
                    usage:       CompletionUsage {
                        input_tokens:  result.usage.input_tokens,
                        output_tokens: result.usage.output_tokens,
                    },
                    output:      result.output,
                })
                .into_response(),
                Err(e) => ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                    .into_response(),
            }
        } else {
            match client.complete(&request).await {
                Ok(response) => Json(CompletionResponse {
                    id:          response.id,
                    model:       response.model,
                    message:     convert_llm_message(&response.message),
                    stop_reason: finish_reason_to_api_stop_reason(&response.finish_reason),
                    usage:       CompletionUsage {
                        input_tokens:  response.usage.input_tokens,
                        output_tokens: response.usage.output_tokens,
                    },
                    output:      None,
                })
                .into_response(),
                Err(e) => ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                    .into_response(),
            }
        }
    }
}
