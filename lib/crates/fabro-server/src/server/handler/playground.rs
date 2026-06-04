//! Playground chat endpoint.
//!
//! POST /api/v1/playground/chat drives a single turn of the chat-driven
//! workflow builder at /playground in fabro-web. The server is stateless
//! across turns: the browser owns the workflow draft and submits it as
//! the literal `workflow.fabro` contents with every request; the server
//! embeds the file in the system prompt, runs the LLM with a single
//! file-write tool surface, and streams the result back over SSE. The
//! browser parses the emitted `workflow.fabro` content, diffs it against
//! its current draft, and animates the resulting changes into the canvas.

use std::sync::Arc;

use serde_json::json;

use super::super::{
    ApiError, AppState, CreatePlaygroundChatRequest, IntoResponse, Json, LlmMessage, LlmRequest,
    RequiredUser, Response, Router, State, StatusCode, ToolChoice, ToolDefinition, error, info,
    post, warn,
};
use super::llm_sse;

/// Sanity caps on a playground chat request. Axum's default 2 MB body
/// limit already catches gigabyte payloads at the framework layer; these
/// add cheap, descriptive 400s before we touch the LLM so a misbehaving
/// or malicious client can't drag a multi-megabyte transcript through
/// streaming + token-billing.
const MAX_MESSAGES_PER_TURN: usize = 50;
/// Generous ceiling for the submitted `workflow.fabro` text: the canvas
/// caps out around a hundred nodes, which renders to roughly 12 KB of
/// DOT.
const MAX_WORKFLOW_FABRO_BYTES: usize = 32 * 1024;

fn validate_request(req: &CreatePlaygroundChatRequest) -> Result<(), ApiError> {
    if req.messages.len() > MAX_MESSAGES_PER_TURN {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "Conversation too long: {} messages (limit {MAX_MESSAGES_PER_TURN}). \
                 Start a new playground session.",
                req.messages.len(),
            ),
        ));
    }
    if req.workflow_fabro.len() > MAX_WORKFLOW_FABRO_BYTES {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "Workflow file too large: {} bytes (limit {MAX_WORKFLOW_FABRO_BYTES}).",
                req.workflow_fabro.len(),
            ),
        ));
    }
    Ok(())
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/playground/chat", post(create_playground_chat))
}

/// System prompt template. The `{workflow_fabro}` placeholder receives
/// the literal `workflow.fabro` contents submitted with the request.
const SYSTEM_PROMPT_TEMPLATE: &str = include_str!("prompts/playground_system.md");

const WORKFLOW_FABRO_PLACEHOLDER: &str = "{workflow_fabro}";

fn build_system_prompt(workflow_fabro: &str) -> String {
    SYSTEM_PROMPT_TEMPLATE.replace(WORKFLOW_FABRO_PLACEHOLDER, workflow_fabro)
}

/// The single file-write tool the model uses to update the workflow.
/// Each turn the model emits one call with the full new contents of
/// `workflow.fabro`. The browser parses the content, diffs it against
/// its current draft, and animates the resulting changes into the
/// canvas.
fn playground_tools() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name:        "write_workflow_file".into(),
        description: "Write the full new contents of a workflow file. For the playground, only \
                      `workflow.fabro` is meaningful — the model emits the complete DOT for the \
                      current desired state of the workflow. The previous file is replaced \
                      atomically; always include every node and edge, not just changes."
            .into(),
        parameters:  json!({
            "type": "object",
            "required": ["file_name", "content"],
            "properties": {
                "file_name": {
                    "type": "string",
                    "enum": ["workflow.fabro"],
                    "description": "Target file name. Currently only `workflow.fabro` is supported."
                },
                "content": {
                    "type": "string",
                    "description": "Full DOT contents of the workflow file. Must be a complete `digraph <name> { ... }` block including `start` and `exit` terminals and every desired node and edge."
                }
            }
        }),
    }]
}

async fn create_playground_chat(
    _auth: RequiredUser,
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreatePlaygroundChatRequest>,
) -> Response {
    if let Err(e) = validate_request(&req) {
        return e.into_response();
    }

    let catalog = state.catalog();
    let model_id = req
        .model
        .unwrap_or_else(|| catalog.default_model().id.clone());

    info!(model = %model_id, "Playground chat turn");

    // Request messages are already the canonical `fabro_types::Message` —
    // the API schema reuses it via build.rs `with_replacement`.
    let mut messages: Vec<LlmMessage> = Vec::new();
    messages.push(LlmMessage::system(build_system_prompt(&req.workflow_fabro)));
    messages.extend(req.messages);

    let request = LlmRequest {
        model: model_id,
        messages,
        provider: None,
        tools: Some(playground_tools()),
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
    };

    let llm_result = match state.resolve_llm_client().await {
        Ok(r) => r,
        Err(err) => {
            error!(error = ?err, "playground: failed to create LLM client");
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create LLM client: {err}"),
            )
            .into_response();
        }
    };
    for (provider, issue) in &llm_result.auth_issues {
        warn!(provider = %provider, error = %issue, "playground: provider auth issue");
    }
    let client = llm_result.client;

    let stream_result = match client.stream(&request).await {
        Ok(s) => s,
        Err(e) => {
            error!(error = ?e, "playground: LLM stream call failed");
            return ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                .into_response();
        }
    };

    // Forward StreamEvents as `stream_event` SSE frames. The browser-side
    // adapter listens for the `tool_call_end` event carrying the
    // `write_workflow_file` arguments, parses the DOT, diffs it against
    // its current draft, and animates the diff into the canvas.
    llm_sse::stream_response(stream_result, state.shutdown_token())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Welcome-state DOT, matching the client renderer's canonical style.
    const WELCOME_DOT: &str = r#"digraph untitled {
    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]

    start -> exit
}
"#;

    #[test]
    fn system_prompt_embeds_workflow_file_verbatim() {
        let dot = r#"digraph release_notes {
    graph [goal="Generate release notes"]
    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare, label="Exit"]
    plan [shape=box, label="Plan", prompt="Plan it"]

    start -> plan
    plan -> exit
}
"#;
        let prompt = build_system_prompt(dot);
        assert!(prompt.contains(dot), "prompt should embed the DOT verbatim");
        assert!(prompt.contains("digraph release_notes"));
        assert!(prompt.contains("write_workflow_file"));
    }

    #[test]
    fn system_prompt_explains_empty_canvas_convention() {
        let prompt = build_system_prompt(WELCOME_DOT);
        assert!(prompt.contains("the canvas is empty"));
        assert!(prompt.contains("digraph snake_case_name"));
        assert!(prompt.contains(WELCOME_DOT));
    }

    #[test]
    fn system_prompt_template_substitutes_its_placeholder() {
        assert_eq!(
            SYSTEM_PROMPT_TEMPLATE
                .matches(WORKFLOW_FABRO_PLACEHOLDER)
                .count(),
            1,
            "template must contain the placeholder exactly once"
        );
        let prompt = build_system_prompt(WELCOME_DOT);
        assert!(
            !prompt.contains(WORKFLOW_FABRO_PLACEHOLDER),
            "placeholder must be substituted away"
        );
    }

    fn make_request(messages_len: usize, workflow_fabro: String) -> CreatePlaygroundChatRequest {
        let messages = (0..messages_len)
            .map(|_| fabro_types::Message {
                role:         fabro_types::Role::User,
                content:      Vec::new(),
                name:         None,
                tool_call_id: None,
            })
            .collect();
        CreatePlaygroundChatRequest {
            messages,
            workflow_fabro,
            model: None,
        }
    }

    #[test]
    fn validate_rejects_oversize_message_history() {
        let req = make_request(MAX_MESSAGES_PER_TURN + 1, WELCOME_DOT.to_string());
        let err = validate_request(&req).expect_err("expected too-many-messages error");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_rejects_oversize_workflow_file() {
        let req = make_request(1, "x".repeat(MAX_WORKFLOW_FABRO_BYTES + 1));
        let err = validate_request(&req).expect_err("expected too-large-file error");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_accepts_normal_sized_requests() {
        let req = make_request(10, WELCOME_DOT.to_string());
        assert!(validate_request(&req).is_ok());
    }

    #[test]
    fn tool_surface_is_single_file_write_tool() {
        let tools = playground_tools();
        assert_eq!(tools.len(), 1, "expected exactly one tool");
        let tool = &tools[0];
        assert_eq!(tool.name, "write_workflow_file");
        let params = serde_json::to_value(&tool.parameters).expect("serialize params");
        let required = params
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        let required_names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_names.contains(&"file_name"));
        assert!(required_names.contains(&"content"));
    }
}
