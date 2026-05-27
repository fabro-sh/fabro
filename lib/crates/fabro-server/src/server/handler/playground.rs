//! Playground chat endpoint.
//!
//! POST /api/v1/playground/chat drives a single turn of the chat-driven
//! workflow builder at /playground in fabro-web. The server is stateless
//! across turns: the browser owns the workflow draft and sends the full
//! state with every request, the server runs the LLM with a fixed
//! pure-write tool surface, and tool calls stream back over SSE for the
//! client to apply to its local reducer.

use std::sync::Arc;

use serde_json::json;

use super::super::{
    ApiError, AppState, CompletionContentPart, CompletionMessage, CompletionMessageRole,
    ContentPart, CreatePlaygroundChatRequest, Duration, Event, IntoResponse, Json, KeepAlive,
    LlmMessage, LlmRequest, PlaygroundWorkflowDraft, RequiredUser, Response, Role, Router, Sse,
    State, StatusCode, ToolChoice, ToolDefinition, error, info, post, warn,
};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/playground/chat", post(create_playground_chat))
}

/// Renders the workflow draft into a compact textual summary the model can
/// reason about. Same format we'd write to `workflow.fabro` if we
/// committed the state, minus theming/comments.
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
        "You are Ask Fabro, helping the user build a Fabro workflow inside the \
         /playground builder. Fabro workflows are Graphviz digraphs where each \
         node's `shape` picks the handler:\n\
         - box: agent (multi-turn LLM with tools — the default)\n\
         - tab: a single LLM call\n\
         - parallelogram: a shell script (use `script` attribute)\n\
         - hexagon: a human gate (pause for review)\n\
         - diamond: a conditional branch (multiple outgoing edges with `condition`)\n\
         - component: fan-out parallel\n\
         - tripleoctagon: merge parallel\n\
         - house: a sub-workflow\n\
         \n\
         You mutate the user's draft only by calling tools — never by writing \
         the DOT yourself. Available tools: set_workflow_meta, add_node, \
         update_node, delete_node, connect, disconnect.\n\
         \n\
         Conventions: snake_case node ids (e.g. `run_tests`, `open_pr`). The \
         `start` and `exit` terminals are reserved — never add/edit/delete \
         them, just `connect` other nodes through them. Pick a clear \
         snake_case name for the workflow as soon as the user's intent is \
         obvious, via set_workflow_meta. Keep prose replies brief — the \
         canvas tells the visual story, you ack what changed.\n\
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

/// The pure-write tool surface exposed to the model. Mirrors the
/// `ToolCall` discriminated union in
/// `app/components/playground/state/reducer.ts` exactly.
fn playground_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name:        "set_workflow_meta".into(),
            description: "Set the workflow's snake_case name and / or its goal sentence.".into(),
            parameters:  json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "snake_case workflow id (e.g. release_notes). Used as `fabro run <name>`."
                    },
                    "goal": {
                        "type": "string",
                        "description": "One-sentence goal for the graph attribute."
                    }
                }
            }),
        },
        ToolDefinition {
            name:        "add_node".into(),
            description: "Add a new node to the draft. Reject snake_case-violating ids and \
                          the reserved ids `start` / `exit`."
                .into(),
            parameters:  json!({
                "type": "object",
                "required": ["id", "label", "shape"],
                "properties": {
                    "id": { "type": "string", "description": "snake_case node id." },
                    "label": { "type": "string", "description": "Human-readable label." },
                    "shape": {
                        "type": "string",
                        "enum": ["box", "tab", "parallelogram", "hexagon", "diamond", "component", "tripleoctagon", "house"],
                        "description": "Fabro shape — picks the node's handler."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Prose body for agent/tab/parallelogram nodes."
                    },
                    "attrs": {
                        "type": "object",
                        "description": "Free-form Graphviz attributes (max_visits, goal_gate, script, timeout, ...)."
                    }
                }
            }),
        },
        ToolDefinition {
            name:        "update_node".into(),
            description: "Update fields on an existing user-added node. Only the supplied \
                          fields change."
                .into(),
            parameters:  json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": { "type": "string" },
                    "label": { "type": "string" },
                    "shape": {
                        "type": "string",
                        "enum": ["box", "tab", "parallelogram", "hexagon", "diamond", "component", "tripleoctagon", "house"]
                    },
                    "prompt": { "type": "string" },
                    "attrs": { "type": "object" }
                }
            }),
        },
        ToolDefinition {
            name:        "delete_node".into(),
            description: "Delete a user-added node and any edges that referenced it. \
                          `start` and `exit` cannot be deleted."
                .into(),
            parameters:  json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": { "type": "string" } }
            }),
        },
        ToolDefinition {
            name:        "connect".into(),
            description: "Add a directed edge between two existing nodes.".into(),
            parameters:  json!({
                "type": "object",
                "required": ["from", "to"],
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" },
                    "condition": {
                        "type": "string",
                        "description": "For diamond branches, e.g. `outcome=succeeded`."
                    },
                    "label": { "type": "string" },
                    "attrs": { "type": "object" }
                }
            }),
        },
        ToolDefinition {
            name:        "disconnect".into(),
            description: "Remove an existing edge between two nodes.".into(),
            parameters:  json!({
                "type": "object",
                "required": ["from", "to"],
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" }
                }
            }),
        },
    ]
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
            return ApiError::new(StatusCode::BAD_GATEWAY, format!("LLM error: {e}"))
                .into_response();
        }
    };

    // Forward StreamEvents as `stream_event` SSE frames — same shape as
    // /api/v1/completions. The browser-side adapter listens for
    // `tool_call_end` events (which carry the parsed args) and applies
    // them to its reducer; `text_delta` events stream into the chat
    // transcript.
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
        Err(e) => Some(Ok(Event::default().event("stream_event").data(
            json!({
                "type": "error",
                "error": {"Stream": {"message": e.to_string()}},
                "raw": null
            })
            .to_string(),
        ))),
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
        assert!(prompt.contains("set_workflow_meta"));
        assert!(prompt.contains("add_node"));
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

    #[test]
    fn tool_surface_covers_every_reducer_action() {
        let tools = playground_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        for expected in [
            "set_workflow_meta",
            "add_node",
            "update_node",
            "delete_node",
            "connect",
            "disconnect",
        ] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
    }
}
