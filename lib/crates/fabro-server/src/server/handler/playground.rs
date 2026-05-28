//! Playground chat endpoint.
//!
//! POST /api/v1/playground/chat drives a single turn of the chat-driven
//! workflow builder at /playground in fabro-web. The server is stateless
//! across turns: the browser owns the workflow draft and sends the full
//! state with every request; the server runs the LLM with a single
//! file-write tool surface and streams the result back over SSE. The
//! browser parses the emitted `workflow.fabro` content, diffs it against
//! its current draft, and animates the resulting changes into the canvas.

use std::sync::Arc;

use serde_json::json;

use super::super::{
    ApiError, AppState, CompletionContentPart, CompletionMessage, CompletionMessageRole,
    ContentPart, CreatePlaygroundChatRequest, Duration, Event, IntoResponse, Json, KeepAlive,
    LlmMessage, LlmRequest, PlaygroundWorkflowDraft, RequiredUser, Response, Role, Router, Sse,
    State, StatusCode, ToolChoice, ToolDefinition, error, info, post, warn,
};

/// Sanity caps on a playground chat request. Axum's default 2 MB body
/// limit already catches gigabyte payloads at the framework layer; these
/// add cheap, descriptive 400s before we touch the LLM so a misbehaving
/// or malicious client can't drag a multi-megabyte transcript through
/// streaming + token-billing.
const MAX_MESSAGES_PER_TURN: usize = 50;
const MAX_WORKFLOW_NODES: usize = 100;
const MAX_WORKFLOW_EDGES: usize = 200;

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
    if req.workflow.nodes.len() > MAX_WORKFLOW_NODES {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "Workflow too large: {} nodes (limit {MAX_WORKFLOW_NODES}).",
                req.workflow.nodes.len(),
            ),
        ));
    }
    if req.workflow.edges.len() > MAX_WORKFLOW_EDGES {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "Workflow too large: {} edges (limit {MAX_WORKFLOW_EDGES}).",
                req.workflow.edges.len(),
            ),
        ));
    }
    Ok(())
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/playground/chat", post(create_playground_chat))
}

/// Renders the workflow draft as a compact DOT-style summary the model
/// can read for current state. Same shape as the file the model writes
/// back via `write_workflow_file`, minus theming/comments.
fn summarize_workflow(workflow: &PlaygroundWorkflowDraft) -> String {
    if workflow
        .nodes
        .iter()
        .all(|n| n.id == "start" || n.id == "exit")
    {
        return "(empty — only `start` and `exit` exist; no user nodes yet)".to_string();
    }
    let mut out = String::new();
    for node in &workflow.nodes {
        if node.id == "start" || node.id == "exit" {
            continue;
        }
        let prompt = node
            .prompt
            .as_deref()
            .map(|p| format!(" prompt={p:?}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {id} [shape={shape} label={label:?}{prompt}]\n",
            id = node.id,
            shape = node.shape,
            label = node.label,
        ));
    }
    out.push('\n');
    for edge in &workflow.edges {
        let label = edge
            .label
            .as_deref()
            .map(|l| format!(" label={l:?}"))
            .unwrap_or_default();
        let cond = edge
            .condition
            .as_deref()
            .map(|c| format!(" condition={c:?}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {from} -> {to}{label}{cond}\n",
            from = edge.from,
            to = edge.to,
        ));
    }
    out
}

fn build_system_prompt(workflow: &PlaygroundWorkflowDraft) -> String {
    let name = if workflow.name.is_empty() || workflow.name == "untitled" {
        "(unnamed)".to_string()
    } else {
        workflow.name.clone()
    };
    let goal = if workflow.goal.is_empty() {
        "(not set yet)".to_string()
    } else {
        workflow.goal.clone()
    };

    format!(
        "You are Ask Fabro, helping the user build a Fabro workflow inside \
         the /playground builder. Fabro workflows are Graphviz digraphs where \
         each node's `shape` picks the handler:\n\
         - box: agent (multi-turn LLM with tools — the default)\n\
         - tab: a single LLM call\n\
         - parallelogram: a shell script (use a `script` attribute)\n\
         - hexagon: a human gate (pause for review)\n\
         - diamond: a conditional branch (multiple outgoing edges with a `condition`)\n\
         - component: fan-out parallel\n\
         - tripleoctagon: merge parallel\n\
         - house: a sub-workflow\n\
         \n\
         To update the workflow, call the `write_workflow_file` tool exactly \
         once per turn with the full new contents of `workflow.fabro`. The \
         file you write REPLACES the previous one — always emit the complete \
         workflow, even nodes and edges that didn't change.\n\
         \n\
         Always include a brief one-line acknowledgement before the tool \
         call so the chat doesn't feel silent — something like \"Built the \
         lint/test/PR pipeline.\" or \"Added the fix-and-retry loop.\". \
         Keep it to one sentence; the canvas shows the details.\n\
         \n\
         DOT template:\n\
         ```\n\
         digraph snake_case_name {{\n\
         \x20   graph [goal=\"One-sentence goal.\"]\n\
         \x20   rankdir=LR\n\
         \n\
         \x20   start [shape=Mdiamond, label=\"Start\"]\n\
         \x20   exit  [shape=Msquare, label=\"Exit\"]\n\
         \n\
         \x20   plan [shape=box, label=\"Plan\", prompt=\"Plan the work.\"]\n\
         \x20   implement [shape=box, label=\"Implement\", prompt=\"...\"]\n\
         \n\
         \x20   start -> plan\n\
         \x20   plan -> implement\n\
         \x20   implement -> exit\n\
         }}\n\
         ```\n\
         \n\
         Rules:\n\
         - snake_case node ids (e.g. `run_tests`, `open_pr`).\n\
         - `start` (shape=Mdiamond) and `exit` (shape=Msquare) are reserved \
         terminals — always present, never renamed, never have prompts.\n\
         - Pick a clear snake_case name for the digraph (the `digraph <name>` \
         token) as soon as the user's intent is obvious.\n\
         - Preserve existing node ids across turns. Only invent a new id for \
         a genuinely new node — don't rename `lint` to `lint_step` just \
         because you're regenerating the file.\n\
         - Every user-added node must be on a path from `start` to `exit`.\n\
         - For `diamond` branches, give each outgoing edge a `condition` \
         attribute (e.g. `gate -> happy_path [condition=\"outcome=approved\"]`).\n\
         - Escape `\\` and `\"` inside attribute strings.\n\
         \n\
         Current draft\n\
         -------------\n\
         name: {name}\n\
         goal: {goal}\n\
         \n\
         {summary}",
        summary = summarize_workflow(workflow),
    )
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
        .filter_map(|part: &CompletionContentPart| {
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

    let mut messages: Vec<LlmMessage> = Vec::new();
    messages.push(LlmMessage::system(build_system_prompt(&req.workflow)));
    for m in &req.messages {
        messages.push(convert_api_message(m));
    }

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
    let sse_stream = tokio_stream::StreamExt::filter_map(stream_result, |event| match event {
        Ok(ref evt) => match serde_json::to_string(evt) {
            Ok(json) => Some(Ok::<_, std::convert::Infallible>(
                Event::default().event("stream_event").data(json),
            )),
            Err(e) => Some(Ok(Event::default().event("stream_event").data(
                json!({
                    "type": "error",
                    "error": {"Stream": {"message": format!("failed to serialize event: {e}")}},
                    "raw": null
                })
                .to_string(),
            ))),
        },
        Err(e) => {
            error!(error = %e, "playground: stream event error");
            Some(Ok(Event::default().event("stream_event").data(
                json!({
                    "type": "error",
                    "error": {"Stream": {"message": e.to_string()}},
                    "raw": null
                })
                .to_string(),
            )))
        }
    });
    let sse_stream =
        futures_util::StreamExt::take_until(sse_stream, state.shutdown_token().cancelled_owned());

    Sse::new(sse_stream)
        .keep_alive(
            KeepAlive::new().interval(Duration::from_secs(15)).event(
                Event::default()
                    .event("ping")
                    .data(json!({"type": "ping"}).to_string()),
            ),
        )
        .into_response()
}

#[cfg(test)]
mod tests {
    use fabro_api::types::{PlaygroundWorkflowEdge, PlaygroundWorkflowNode};

    use super::*;

    fn empty_draft() -> PlaygroundWorkflowDraft {
        PlaygroundWorkflowDraft {
            name:  "untitled".into(),
            goal:  String::new(),
            nodes: vec![
                PlaygroundWorkflowNode {
                    id:     "start".into(),
                    label:  "Start".into(),
                    shape:  "mdiamond".into(),
                    prompt: None,
                    attrs:  serde_json::Map::new(),
                },
                PlaygroundWorkflowNode {
                    id:     "exit".into(),
                    label:  "Exit".into(),
                    shape:  "msquare".into(),
                    prompt: None,
                    attrs:  serde_json::Map::new(),
                },
            ],
            edges: vec![PlaygroundWorkflowEdge {
                from:      "start".into(),
                to:        "exit".into(),
                label:     None,
                condition: None,
                attrs:     serde_json::Map::new(),
            }],
        }
    }

    #[test]
    fn system_prompt_calls_out_empty_state_for_welcome_draft() {
        let prompt = build_system_prompt(&empty_draft());
        assert!(prompt.contains("(empty —"));
        assert!(prompt.contains("(unnamed)"));
        assert!(prompt.contains("write_workflow_file"));
        assert!(prompt.contains("digraph snake_case_name"));
    }

    #[test]
    fn system_prompt_includes_user_nodes_and_edges() {
        let mut draft = empty_draft();
        draft.name = "release_notes".into();
        draft.goal = "Generate release notes".into();
        draft.nodes.push(PlaygroundWorkflowNode {
            id:     "plan".into(),
            label:  "Plan".into(),
            shape:  "box".into(),
            prompt: Some("Plan it".into()),
            attrs:  serde_json::Map::new(),
        });
        draft.edges.push(PlaygroundWorkflowEdge {
            from:      "start".into(),
            to:        "plan".into(),
            label:     None,
            condition: None,
            attrs:     serde_json::Map::new(),
        });
        let prompt = build_system_prompt(&draft);
        assert!(prompt.contains("name: release_notes"));
        assert!(prompt.contains("goal: Generate release notes"));
        assert!(prompt.contains("plan [shape=box"));
        assert!(prompt.contains("start -> plan"));
    }

    fn make_request(
        messages_len: usize,
        nodes_len: usize,
        edges_len: usize,
    ) -> CreatePlaygroundChatRequest {
        use fabro_api::types::{CompletionMessage, CompletionMessageRole};
        let messages = (0..messages_len)
            .map(|_| CompletionMessage {
                role:         CompletionMessageRole::User,
                content:      Vec::new(),
                name:         None,
                tool_call_id: None,
            })
            .collect();
        let mut workflow = empty_draft();
        while workflow.nodes.len() < nodes_len {
            let i = workflow.nodes.len();
            workflow.nodes.push(PlaygroundWorkflowNode {
                id:     format!("n{i}"),
                label:  format!("N{i}"),
                shape:  "box".into(),
                prompt: None,
                attrs:  serde_json::Map::new(),
            });
        }
        while workflow.edges.len() < edges_len {
            workflow.edges.push(PlaygroundWorkflowEdge {
                from:      "start".into(),
                to:        "exit".into(),
                label:     None,
                condition: None,
                attrs:     serde_json::Map::new(),
            });
        }
        CreatePlaygroundChatRequest {
            messages,
            workflow,
            model: None,
        }
    }

    #[test]
    fn validate_rejects_oversize_message_history() {
        let req = make_request(MAX_MESSAGES_PER_TURN + 1, 2, 1);
        let err = validate_request(&req).expect_err("expected too-many-messages error");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_rejects_oversize_workflow() {
        let req = make_request(1, MAX_WORKFLOW_NODES + 1, 1);
        let err = validate_request(&req).expect_err("expected too-many-nodes error");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_accepts_normal_sized_requests() {
        let req = make_request(10, 8, 12);
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
