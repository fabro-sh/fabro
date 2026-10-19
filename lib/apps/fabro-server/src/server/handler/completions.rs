use std::sync::Arc;

use fabro_llm::{ModelSelection, Request};
use lithos_llm::types::{Message, Role};

use super::super::{
    ApiError, AppState, CreateCompletionRequest, IntoResponse, Json, ProviderId, RequiredUser,
    Response, Router, State, StatusCode, error, info, post, warn,
};
use super::llm_sse;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/completions", post(create_completion))
}

async fn create_completion(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateCompletionRequest>,
) -> Response {
    let llm_result = match state.resolve_llm_client().await {
        Ok(result) => result,
        Err(err) => {
            error!(error = ?err, "Failed to create LLM client");
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to resolve LLM providers: {err}"),
            )
            .into_response();
        }
    };
    for (provider, issue) in &llm_result.auth_issues {
        warn!(provider = %provider, error = %issue, "LLM provider unavailable due to auth issue");
    }
    for issue in &llm_result.build_issues {
        warn!(provider = %issue.provider, error = %issue.cause, "LLM provider unavailable due to build issue");
    }
    let client = llm_result.client;
    let selection = ModelSelection::from_parts(req.model.as_deref(), req.provider.as_deref());
    let route = match client.resolve_selection(&selection) {
        Ok(route) => route,
        Err(error) => return ApiError::bad_request(error.to_string()).into_response(),
    };
    let mut builder = Request::builder().selection(selection);
    if let Some(system) = req.system {
        builder = builder.message(Message::text(Role::System, system));
    }
    for message in req.messages {
        builder = builder.message(message);
    }
    for tool in req.tools {
        builder = builder.tool(tool);
    }
    if let Some(choice) = req.tool_choice {
        builder = builder.tool_choice(choice);
    }
    if let Some(format) = req.response_format {
        builder = builder.response_format(format);
    }
    if let Some(max_output_tokens) = req.max_output_tokens {
        match u32::try_from(max_output_tokens) {
            Ok(tokens) => builder = builder.max_output_tokens(tokens),
            Err(_) => {
                return ApiError::bad_request("max_output_tokens is out of range").into_response();
            }
        }
    }
    if let Some(temperature) = req.temperature {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "Sampling parameters are low-precision by nature."
        )]
        {
            builder = builder.temperature(temperature as f32);
        }
    }
    if let Some(top_p) = req.top_p {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "Sampling parameters are low-precision by nature."
        )]
        {
            builder = builder.top_p(top_p as f32);
        }
    }
    if !req.stop_sequences.is_empty() {
        builder = builder.stop_sequences(req.stop_sequences);
    }
    if let Some(effort) = req.reasoning_effort {
        builder = builder.reasoning_effort(effort);
    }
    if let Some(speed) = req.speed {
        builder = builder.speed(speed);
    }
    for (key, value) in req.metadata {
        builder = builder.metadata_entry(key, value);
    }
    for (provider, options) in req.provider_options {
        let Some(options) = options.as_object() else {
            return ApiError::bad_request(format!(
                "provider_options.{provider} must be a JSON object"
            ))
            .into_response();
        };
        builder = builder.provider_options(ProviderId::new(provider), options.clone());
    }
    let request = match builder.build() {
        Ok(request) => request,
        Err(error) => return ApiError::bad_request(error.to_string()).into_response(),
    };
    info!(
        model = %route.model().id(),
        provider = %route.provider().id(),
        "Completion request received"
    );

    // Structured output is a complete response by construction.
    let use_stream = req.stream && req.schema.is_none();

    if use_stream {
        let stream_result = match client.stream(request).await {
            Ok(stream) => stream,
            Err(error) => return ApiError::from(error).into_response(),
        };
        return llm_sse::stream_response(stream_result, state.shutdown_token());
    }

    if let Some(schema) = req.schema {
        return match client
            .complete_object(request, "output_schema", schema)
            .await
        {
            Ok(completion) => {
                let mut body = match serde_json::to_value(&completion.response) {
                    Ok(body) => body,
                    Err(error) => {
                        return ApiError::new(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("failed to serialize completion: {error}"),
                        )
                        .into_response();
                    }
                };
                body["output"] = completion.object;
                Json(body).into_response()
            }
            Err(error) => ApiError::from(error).into_response(),
        };
    }

    match client.complete(request).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}
