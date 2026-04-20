#![allow(
    clippy::print_stderr,
    reason = "These live contract tests print stderr diagnostics on mismatch."
)]

mod common;

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::{Value, json};

const DEFAULT_LIVE_BASE_URL: &str = "https://api.openai.com";
const DEFAULT_LIVE_MODEL: &str = "gpt-5-nano-2025-08-07";
const LIVE_IMAGE_URL: &str = "https://upload.wikimedia.org/wikipedia/commons/thumb/a/a7/React-icon.svg/120px-React-icon.svg.png";
const RESPONSES_STREAM_MILESTONES: &[&str] = &[
    "response.created",
    "response.in_progress",
    "response.output_item.added",
    "response.content_part.added",
    "response.output_text.delta",
    "response.output_text.done",
    "response.content_part.done",
    "response.function_call_arguments.delta",
    "response.function_call_arguments.done",
    "response.output_item.done",
    "response.completed",
];

#[derive(Clone)]
struct LiveOptions {
    api:   common::ApiClient,
    model: String,
}

#[derive(Clone, Debug)]
enum SurfaceAvailability {
    Available,
    Blocked(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ChatStreamMilestone {
    Role,
    ContentDelta,
    ToolCallDelta,
    FinishStop,
    FinishToolCalls,
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ToolCallObservation {
    name:      String,
    arguments: Value,
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY and outbound network"]
async fn live_openai_contract_smoke_suite() {
    let Some(config) = LiveOptions::from_env().expect("live config should load") else {
        eprintln!("skipping live OpenAI smoke suite because OPENAI_API_KEY is not set");
        return;
    };

    let responses_availability = probe_responses_availability(&config)
        .await
        .expect("responses availability probe should complete");
    let chat_availability = probe_chat_availability(&config)
        .await
        .expect("chat availability probe should complete");

    let mut failures = Vec::new();
    let mut ran_cases = 0_usize;

    match responses_availability {
        SurfaceAvailability::Available => {
            ran_cases += 11;
            collect_failure(
                &mut failures,
                "responses non-stream text",
                compare_responses_non_stream_text(&config).await,
            );
            collect_failure(
                &mut failures,
                "responses stream text",
                compare_responses_stream_text(&config).await,
            );
            collect_failure(
                &mut failures,
                "responses non-stream structured output",
                compare_responses_structured_output(&config).await,
            );
            collect_failure(
                &mut failures,
                "responses stream structured output",
                compare_responses_stream_structured_output(&config).await,
            );
            collect_failure(
                &mut failures,
                "responses non-stream tool call",
                compare_responses_tool_call(&config).await,
            );
            collect_failure(
                &mut failures,
                "responses stream tool call",
                compare_responses_stream_tool_call(&config).await,
            );
            collect_failure(
                &mut failures,
                "responses continuation",
                compare_responses_continuation(&config).await,
            );
            collect_failure(
                &mut failures,
                "responses stream continuation",
                compare_responses_stream_continuation(&config).await,
            );
            collect_failure(
                &mut failures,
                "responses image input",
                compare_responses_image_input(&config).await,
            );
            collect_failure(
                &mut failures,
                "responses stream image input",
                compare_responses_stream_image_input(&config).await,
            );
            collect_failure(
                &mut failures,
                "responses tool_choice none",
                compare_responses_tool_choice_none(&config).await,
            );
        }
        SurfaceAvailability::Blocked(reason) => {
            eprintln!("skipping responses live cases: {reason}");
        }
    }

    match chat_availability {
        SurfaceAvailability::Available => {
            ran_cases += 9;
            collect_failure(
                &mut failures,
                "chat non-stream text",
                compare_chat_non_stream_text(&config).await,
            );
            collect_failure(
                &mut failures,
                "chat stream text",
                compare_chat_stream_text(&config).await,
            );
            collect_failure(
                &mut failures,
                "chat non-stream structured output",
                compare_chat_structured_output(&config).await,
            );
            collect_failure(
                &mut failures,
                "chat stream structured output",
                compare_chat_stream_structured_output(&config).await,
            );
            collect_failure(
                &mut failures,
                "chat non-stream tool call",
                compare_chat_tool_call(&config).await,
            );
            collect_failure(
                &mut failures,
                "chat stream tool call",
                compare_chat_stream_tool_call(&config).await,
            );
            collect_failure(
                &mut failures,
                "chat image input",
                compare_chat_image_input(&config).await,
            );
            collect_failure(
                &mut failures,
                "chat stream image input",
                compare_chat_stream_image_input(&config).await,
            );
            collect_failure(
                &mut failures,
                "chat tool_choice none",
                compare_chat_tool_choice_none(&config).await,
            );
        }
        SurfaceAvailability::Blocked(reason) => {
            eprintln!("skipping chat live cases: {reason}");
        }
    }

    if ran_cases == 0 {
        eprintln!("skipping live OpenAI smoke suite because no live surfaces are accessible");
        return;
    }

    if failures.is_empty() {
        return;
    }

    panic!(
        "live OpenAI smoke suite detected drift:\n{}",
        failures
            .iter()
            .map(|failure| format!("- {failure}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

impl LiveOptions {
    fn from_env() -> Result<Option<Self>> {
        let Some(api_key) = non_empty_env("OPENAI_API_KEY") else {
            return Ok(None);
        };
        let base_url = non_empty_env("TWIN_OPENAI_LIVE_BASE_URL")
            .unwrap_or_else(|| DEFAULT_LIVE_BASE_URL.to_owned());
        let model = non_empty_env("TWIN_OPENAI_LIVE_MODEL")
            .unwrap_or_else(|| DEFAULT_LIVE_MODEL.to_owned());
        let organization = non_empty_env("OPENAI_ORGANIZATION");
        let project = non_empty_env("OPENAI_PROJECT");

        Ok(Some(Self {
            api: common::ApiClient::new(base_url, Some(api_key), organization, project)?,
            model,
        }))
    }
}

fn collect_failure(failures: &mut Vec<String>, name: &str, result: Result<()>) {
    match result {
        Ok(()) => eprintln!("ok: {name}"),
        Err(error) => {
            eprintln!("drift: {name}: {error:#}");
            failures.push(format!("{name}: {error:#}"));
        }
    }
}

async fn probe_responses_availability(config: &LiveOptions) -> Result<SurfaceAvailability> {
    probe_surface_availability(
        &config.api,
        "/v1/responses",
        &json!({
            "model": config.model,
            "input": "ping",
            "stream": false
        }),
        "responses",
    )
    .await
}

async fn probe_chat_availability(config: &LiveOptions) -> Result<SurfaceAvailability> {
    probe_surface_availability(
        &config.api,
        "/v1/chat/completions",
        &json!({
            "model": config.model,
            "messages": [{ "role": "user", "content": "ping" }],
            "stream": false
        }),
        "chat.completions",
    )
    .await
}

async fn probe_surface_availability(
    client: &common::ApiClient,
    path: &str,
    body: &Value,
    label: &str,
) -> Result<SurfaceAvailability> {
    let response = client.post_json_recorded(path, body).await;

    if response.status == fabro_http::StatusCode::OK {
        return Ok(SurfaceAvailability::Available);
    }

    if let Some(reason) = classify_live_access_blocker(&response) {
        return Ok(SurfaceAvailability::Blocked(format!("{label}: {reason}")));
    }

    bail!(
        "{} availability probe returned unexpected status {}: {}",
        label,
        response.status,
        summarize_recorded_body(&response)
    );
}

async fn compare_responses_non_stream_text(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    let request = json!({
        "model": config.model,
        "input": "Reply with a short greeting.",
        "stream": false
    });

    let local = post_json_ok(&server.api_client(), "/v1/responses", &request).await?;
    let live = post_json_ok(&config.api, "/v1/responses", &request).await?;

    ensure_eq(
        local["object"].as_str(),
        live["object"].as_str(),
        "responses object",
    )?;
    ensure_eq(
        Some(has_responses_usage(&local)),
        Some(has_responses_usage(&live)),
        "responses usage presence",
    )?;
    ensure!(
        extract_response_text(&local).is_some(),
        "local responses payload did not include output text: {}",
        truncate_for_display(&local.to_string(), 240)
    );
    ensure!(
        extract_response_text(&live).is_some(),
        "live responses payload did not include output text: {}",
        truncate_for_display(&live.to_string(), 240)
    );

    Ok(())
}

async fn compare_responses_stream_text(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    let request = json!({
        "model": config.model,
        "input": "Reply with a short greeting.",
        "stream": true
    });

    let local = post_sse_ok(&server.api_client(), "/v1/responses", &request).await?;
    let live = post_sse_ok(&config.api, "/v1/responses", &request).await?;

    let local_milestones = normalize_response_stream(&local);
    let live_milestones = normalize_response_stream(&live);

    ensure_eq(
        Some(local_milestones.clone()),
        Some(live_milestones.clone()),
        "responses stream milestones",
    )?;
    ensure_eq(
        Some(local.done),
        Some(live.done),
        "responses stream done marker",
    )?;

    Ok(())
}

async fn compare_responses_structured_output(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    let schema = smoke_schema();
    let request = json!({
        "model": config.model,
        "input": "Return a short structured answer.",
        "stream": false,
        "text": {
            "format": {
                "type": "json_schema",
                "name": "smoke_schema",
                "schema": schema,
                "strict": true
            }
        }
    });

    let local = post_json_ok(&server.api_client(), "/v1/responses", &request).await?;
    let live = post_json_ok(&config.api, "/v1/responses", &request).await?;

    let local_json = extract_response_structured_json(&local)
        .context("local responses payload did not contain structured JSON")?;
    let live_json = extract_response_structured_json(&live)
        .context("live responses payload did not contain structured JSON")?;

    ensure!(
        value_matches_schema(&local_json, &schema),
        "local structured JSON did not match schema: {}",
        truncate_for_display(&local_json.to_string(), 240)
    );
    ensure!(
        value_matches_schema(&live_json, &schema),
        "live structured JSON did not match schema: {}",
        truncate_for_display(&live_json.to_string(), 240)
    );

    Ok(())
}

async fn compare_responses_stream_structured_output(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    let schema = smoke_schema();
    let request = json!({
        "model": config.model,
        "input": "Return a short structured answer.",
        "stream": true,
        "text": {
            "format": {
                "type": "json_schema",
                "name": "smoke_schema",
                "schema": schema,
                "strict": true
            }
        }
    });

    let local = post_sse_ok(&server.api_client(), "/v1/responses", &request).await?;
    let live = post_sse_ok(&config.api, "/v1/responses", &request).await?;

    ensure_eq(
        Some(normalize_response_stream(&local)),
        Some(normalize_response_stream(&live)),
        "responses structured stream milestones",
    )?;
    ensure_eq(
        Some(local.done),
        Some(live.done),
        "responses structured stream done marker",
    )?;

    let local_json = extract_response_stream_structured_json(&local)?
        .context("local responses structured stream did not contain JSON output")?;
    let live_json = extract_response_stream_structured_json(&live)?
        .context("live responses structured stream did not contain JSON output")?;

    ensure!(
        value_matches_schema(&local_json, &schema),
        "local structured stream JSON did not match schema: {}",
        truncate_for_display(&local_json.to_string(), 240)
    );
    ensure!(
        value_matches_schema(&live_json, &schema),
        "live structured stream JSON did not match schema: {}",
        truncate_for_display(&live_json.to_string(), 240)
    );

    Ok(())
}

async fn compare_responses_tool_call(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    enqueue_tool_scenario(&server, "responses", &config.model, false).await;

    let request = json!({
        "model": config.model,
        "input": "Use the weather tool for Boston.",
        "stream": false,
        "tools": [{
            "type": "function",
            "name": "lookup_weather",
            "description": "Look up the weather",
            "parameters": tool_parameters_schema(),
            "strict": true
        }],
        "tool_choice": {
            "type": "function",
            "name": "lookup_weather"
        }
    });

    let local = server
        .api_client()
        .post_json_recorded("/v1/responses", &request)
        .await;
    let live = config
        .api
        .post_json_recorded("/v1/responses", &request)
        .await;

    ensure_matching_status(&local, &live, "responses tool call")?;

    let local_json = parse_json_body(&local)?;
    let live_json = parse_json_body(&live)?;
    let local_tool = extract_response_tool_call(&local_json)
        .context("local responses payload did not contain a function_call output item")?;
    let live_tool = extract_response_tool_call(&live_json)
        .context("live responses payload did not contain a function_call output item")?;

    ensure_eq(
        Some(local_tool.name.clone()),
        Some(live_tool.name.clone()),
        "responses tool name",
    )?;
    ensure!(
        value_matches_schema(&local_tool.arguments, &tool_parameters_schema()),
        "local responses tool arguments did not match schema: {}",
        truncate_for_display(&local_tool.arguments.to_string(), 240)
    );
    ensure!(
        value_matches_schema(&live_tool.arguments, &tool_parameters_schema()),
        "live responses tool arguments did not match schema: {}",
        truncate_for_display(&live_tool.arguments.to_string(), 240)
    );

    Ok(())
}

async fn compare_responses_stream_tool_call(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    enqueue_tool_scenario(&server, "responses", &config.model, true).await;

    let request = json!({
        "model": config.model,
        "input": "Use the weather tool for Boston.",
        "stream": true,
        "tools": [{
            "type": "function",
            "name": "lookup_weather",
            "description": "Look up the weather",
            "parameters": tool_parameters_schema(),
            "strict": true
        }],
        "tool_choice": {
            "type": "function",
            "name": "lookup_weather"
        }
    });

    let local = post_sse_ok(&server.api_client(), "/v1/responses", &request).await?;
    let live = post_sse_ok(&config.api, "/v1/responses", &request).await?;

    ensure_eq(
        Some(normalize_response_stream(&local)),
        Some(normalize_response_stream(&live)),
        "responses tool stream milestones",
    )?;
    ensure_eq(
        Some(local.done),
        Some(live.done),
        "responses tool stream done marker",
    )?;

    let local_tool = extract_response_stream_tool_call(&local)?
        .context("local responses tool stream did not contain a function call")?;
    let live_tool = extract_response_stream_tool_call(&live)?
        .context("live responses tool stream did not contain a function call")?;

    ensure_eq(
        Some(local_tool.name.clone()),
        Some(live_tool.name.clone()),
        "responses tool stream name",
    )?;
    ensure!(
        value_matches_schema(&local_tool.arguments, &tool_parameters_schema()),
        "local responses tool stream arguments did not match schema: {}",
        truncate_for_display(&local_tool.arguments.to_string(), 240)
    );
    ensure!(
        value_matches_schema(&live_tool.arguments, &tool_parameters_schema()),
        "live responses tool stream arguments did not match schema: {}",
        truncate_for_display(&live_tool.arguments.to_string(), 240)
    );

    Ok(())
}

async fn compare_responses_continuation(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    enqueue_responses_continuation_scenarios(&server, &config.model).await;

    let first_request = json!({
        "model": config.model,
        "input": "Use the weather tool for Boston.",
        "stream": false,
        "tools": [{
            "type": "function",
            "name": "lookup_weather",
            "description": "Look up the weather",
            "parameters": tool_parameters_schema(),
            "strict": true
        }],
        "tool_choice": {
            "type": "function",
            "name": "lookup_weather"
        }
    });

    let local_first = post_json_ok(&server.api_client(), "/v1/responses", &first_request).await?;
    let live_first = post_json_ok(&config.api, "/v1/responses", &first_request).await?;

    let local_tool = extract_response_tool_call(&local_first)
        .context("local first continuation turn did not contain function_call output")?;
    let live_tool = extract_response_tool_call(&live_first)
        .context("live first continuation turn did not contain function_call output")?;

    ensure_eq(
        Some(local_tool.name.clone()),
        Some(live_tool.name.clone()),
        "responses continuation tool name",
    )?;
    ensure!(
        value_matches_schema(&local_tool.arguments, &tool_parameters_schema()),
        "local continuation tool arguments did not match schema: {}",
        truncate_for_display(&local_tool.arguments.to_string(), 240)
    );
    ensure!(
        value_matches_schema(&live_tool.arguments, &tool_parameters_schema()),
        "live continuation tool arguments did not match schema: {}",
        truncate_for_display(&live_tool.arguments.to_string(), 240)
    );

    let local_response_id = local_first
        .get("id")
        .and_then(Value::as_str)
        .context("local first continuation turn did not contain response id")?;
    let live_response_id = live_first
        .get("id")
        .and_then(Value::as_str)
        .context("live first continuation turn did not contain response id")?;
    let local_call_id = extract_response_tool_call_id(&local_first)
        .context("local first continuation turn did not contain call_id")?;
    let live_call_id = extract_response_tool_call_id(&live_first)
        .context("live first continuation turn did not contain call_id")?;

    let local_second = post_json_ok(
        &server.api_client(),
        "/v1/responses",
        &json!({
            "model": config.model,
            "stream": false,
            "previous_response_id": local_response_id,
            "input": [{
                "type": "function_call_output",
                "call_id": local_call_id,
                "output": "72 and sunny"
            }]
        }),
    )
    .await?;
    let live_second = post_json_ok(
        &config.api,
        "/v1/responses",
        &json!({
            "model": config.model,
            "stream": false,
            "previous_response_id": live_response_id,
            "input": [{
                "type": "function_call_output",
                "call_id": live_call_id,
                "output": "72 and sunny"
            }]
        }),
    )
    .await?;

    ensure!(
        extract_response_text(&local_second).is_some(),
        "local continuation response did not contain assistant text: {}",
        truncate_for_display(&local_second.to_string(), 240)
    );
    ensure!(
        extract_response_text(&live_second).is_some(),
        "live continuation response did not contain assistant text: {}",
        truncate_for_display(&live_second.to_string(), 240)
    );

    Ok(())
}

async fn compare_responses_stream_continuation(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    enqueue_responses_continuation_scenarios(&server, &config.model).await;

    let first_request = json!({
        "model": config.model,
        "input": "Use the weather tool for Boston.",
        "stream": false,
        "tools": [{
            "type": "function",
            "name": "lookup_weather",
            "description": "Look up the weather",
            "parameters": tool_parameters_schema(),
            "strict": true
        }],
        "tool_choice": {
            "type": "function",
            "name": "lookup_weather"
        }
    });

    let local_first = post_json_ok(&server.api_client(), "/v1/responses", &first_request).await?;
    let live_first = post_json_ok(&config.api, "/v1/responses", &first_request).await?;

    let local_response_id = local_first
        .get("id")
        .and_then(Value::as_str)
        .context("local first streamed continuation turn did not contain response id")?;
    let live_response_id = live_first
        .get("id")
        .and_then(Value::as_str)
        .context("live first streamed continuation turn did not contain response id")?;
    let local_call_id = extract_response_tool_call_id(&local_first)
        .context("local first streamed continuation turn did not contain call_id")?;
    let live_call_id = extract_response_tool_call_id(&live_first)
        .context("live first streamed continuation turn did not contain call_id")?;

    let local_stream = post_sse_ok(
        &server.api_client(),
        "/v1/responses",
        &json!({
            "model": config.model,
            "stream": true,
            "previous_response_id": local_response_id,
            "input": [{
                "type": "function_call_output",
                "call_id": local_call_id,
                "output": "72 and sunny"
            }]
        }),
    )
    .await?;
    let live_stream = post_sse_ok(
        &config.api,
        "/v1/responses",
        &json!({
            "model": config.model,
            "stream": true,
            "previous_response_id": live_response_id,
            "input": [{
                "type": "function_call_output",
                "call_id": live_call_id,
                "output": "72 and sunny"
            }]
        }),
    )
    .await?;

    ensure_eq(
        Some(normalize_response_stream(&local_stream)),
        Some(normalize_response_stream(&live_stream)),
        "responses continuation stream milestones",
    )?;
    ensure_eq(
        Some(local_stream.done),
        Some(live_stream.done),
        "responses continuation stream done marker",
    )?;
    ensure!(
        extract_response_stream_text(&local_stream)?.is_some(),
        "local streamed continuation response did not contain assistant text"
    );
    ensure!(
        extract_response_stream_text(&live_stream)?.is_some(),
        "live streamed continuation response did not contain assistant text"
    );

    Ok(())
}

async fn compare_responses_image_input(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    let request = json!({
        "model": config.model,
        "stream": false,
        "input": [{
            "role": "user",
            "content": [
                { "type": "input_text", "text": "Describe this image briefly." },
                { "type": "input_image", "image_url": LIVE_IMAGE_URL }
            ]
        }]
    });

    let local = post_json_ok(&server.api_client(), "/v1/responses", &request).await?;
    let live = post_json_ok(&config.api, "/v1/responses", &request).await?;

    ensure_eq(
        local["object"].as_str(),
        live["object"].as_str(),
        "responses image object",
    )?;
    ensure!(
        extract_response_text(&local).is_some(),
        "local responses image payload did not include assistant text: {}",
        truncate_for_display(&local.to_string(), 240)
    );
    ensure!(
        extract_response_text(&live).is_some(),
        "live responses image payload did not include assistant text: {}",
        truncate_for_display(&live.to_string(), 240)
    );

    Ok(())
}

async fn compare_responses_stream_image_input(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    let request = json!({
        "model": config.model,
        "stream": true,
        "input": [{
            "role": "user",
            "content": [
                { "type": "input_text", "text": "Describe this image briefly." },
                { "type": "input_image", "image_url": LIVE_IMAGE_URL }
            ]
        }]
    });

    let local = post_sse_ok(&server.api_client(), "/v1/responses", &request).await?;
    let live = post_sse_ok(&config.api, "/v1/responses", &request).await?;

    ensure_eq(
        Some(normalize_response_stream(&local)),
        Some(normalize_response_stream(&live)),
        "responses image stream milestones",
    )?;
    ensure_eq(
        Some(local.done),
        Some(live.done),
        "responses image stream done marker",
    )?;
    ensure!(
        extract_response_stream_text(&local)?.is_some(),
        "local responses image stream did not include assistant text"
    );
    ensure!(
        extract_response_stream_text(&live)?.is_some(),
        "live responses image stream did not include assistant text"
    );

    Ok(())
}

async fn compare_responses_tool_choice_none(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    let request = json!({
        "model": config.model,
        "input": "Reply with a short greeting without using tools.",
        "stream": false,
        "tools": [{
            "type": "function",
            "name": "lookup_weather",
            "description": "Look up the weather",
            "parameters": tool_parameters_schema(),
            "strict": true
        }],
        "tool_choice": "none"
    });

    let local = post_json_ok(&server.api_client(), "/v1/responses", &request).await?;
    let live = post_json_ok(&config.api, "/v1/responses", &request).await?;

    ensure!(
        extract_response_text(&local).is_some(),
        "local responses tool_choice none did not include assistant text"
    );
    ensure!(
        extract_response_text(&live).is_some(),
        "live responses tool_choice none did not include assistant text"
    );
    ensure_eq(
        Some(extract_response_tool_call(&local).is_some()),
        Some(extract_response_tool_call(&live).is_some()),
        "responses tool_choice none tool call presence",
    )?;
    ensure!(
        extract_response_tool_call(&local).is_none(),
        "local responses tool_choice none unexpectedly returned tool calls: {}",
        truncate_for_display(&local.to_string(), 240)
    );
    ensure!(
        extract_response_tool_call(&live).is_none(),
        "live responses tool_choice none unexpectedly returned tool calls: {}",
        truncate_for_display(&live.to_string(), 240)
    );

    Ok(())
}

async fn compare_chat_non_stream_text(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    let request = json!({
        "model": config.model,
        "messages": [{ "role": "user", "content": "Reply with a short greeting." }],
        "stream": false
    });

    let local = post_json_ok(&server.api_client(), "/v1/chat/completions", &request).await?;
    let live = post_json_ok(&config.api, "/v1/chat/completions", &request).await?;

    ensure_eq(
        local["object"].as_str(),
        live["object"].as_str(),
        "chat object",
    )?;
    ensure_eq(
        Some(has_chat_usage(&local)),
        Some(has_chat_usage(&live)),
        "chat usage presence",
    )?;
    ensure!(
        extract_chat_text(&local).is_some(),
        "local chat payload did not include assistant text: {}",
        truncate_for_display(&local.to_string(), 240)
    );
    ensure!(
        extract_chat_text(&live).is_some(),
        "live chat payload did not include assistant text: {}",
        truncate_for_display(&live.to_string(), 240)
    );

    Ok(())
}

async fn compare_chat_stream_text(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    let request = json!({
        "model": config.model,
        "messages": [{ "role": "user", "content": "Reply with a short greeting." }],
        "stream": true
    });

    let local = post_sse_ok(&server.api_client(), "/v1/chat/completions", &request).await?;
    let live = post_sse_ok(&config.api, "/v1/chat/completions", &request).await?;

    let local_milestones = normalize_chat_stream(&local)?;
    let live_milestones = normalize_chat_stream(&live)?;

    ensure_eq(
        Some(local_milestones),
        Some(live_milestones),
        "chat stream milestones",
    )?;

    Ok(())
}

async fn compare_chat_structured_output(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    let schema = smoke_schema();
    let request = json!({
        "model": config.model,
        "messages": [{ "role": "user", "content": "Return a short structured answer." }],
        "stream": false,
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "smoke_schema",
                "schema": schema,
                "strict": true
            }
        }
    });

    let local = post_json_ok(&server.api_client(), "/v1/chat/completions", &request).await?;
    let live = post_json_ok(&config.api, "/v1/chat/completions", &request).await?;

    let local_json = extract_chat_structured_json(&local)
        .context("local chat payload did not contain structured JSON")?;
    let live_json = extract_chat_structured_json(&live)
        .context("live chat payload did not contain structured JSON")?;

    ensure!(
        value_matches_schema(&local_json, &schema),
        "local chat structured JSON did not match schema: {}",
        truncate_for_display(&local_json.to_string(), 240)
    );
    ensure!(
        value_matches_schema(&live_json, &schema),
        "live chat structured JSON did not match schema: {}",
        truncate_for_display(&live_json.to_string(), 240)
    );

    Ok(())
}

async fn compare_chat_stream_structured_output(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    let schema = smoke_schema();
    let request = json!({
        "model": config.model,
        "messages": [{ "role": "user", "content": "Return a short structured answer." }],
        "stream": true,
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "smoke_schema",
                "schema": schema,
                "strict": true
            }
        }
    });

    let local = post_sse_ok(&server.api_client(), "/v1/chat/completions", &request).await?;
    let live = post_sse_ok(&config.api, "/v1/chat/completions", &request).await?;

    ensure_eq(
        Some(normalize_chat_stream(&local)?),
        Some(normalize_chat_stream(&live)?),
        "chat structured stream milestones",
    )?;

    let local_json = extract_chat_stream_structured_json(&local)?
        .context("local chat structured stream did not contain JSON output")?;
    let live_json = extract_chat_stream_structured_json(&live)?
        .context("live chat structured stream did not contain JSON output")?;

    ensure!(
        value_matches_schema(&local_json, &schema),
        "local chat structured stream JSON did not match schema: {}",
        truncate_for_display(&local_json.to_string(), 240)
    );
    ensure!(
        value_matches_schema(&live_json, &schema),
        "live chat structured stream JSON did not match schema: {}",
        truncate_for_display(&live_json.to_string(), 240)
    );

    Ok(())
}

async fn compare_chat_tool_call(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    enqueue_tool_scenario(&server, "chat.completions", &config.model, false).await;

    let request = json!({
        "model": config.model,
        "messages": [{ "role": "user", "content": "Use the weather tool for Boston." }],
        "stream": false,
        "tools": [{
            "type": "function",
            "function": {
                "name": "lookup_weather",
                "description": "Look up the weather",
                "parameters": tool_parameters_schema()
            }
        }],
        "tool_choice": {
            "type": "function",
            "function": {
                "name": "lookup_weather"
            }
        }
    });

    let local = post_json_ok(&server.api_client(), "/v1/chat/completions", &request).await?;
    let live = post_json_ok(&config.api, "/v1/chat/completions", &request).await?;

    let local_tool =
        extract_chat_tool_call(&local).context("local chat payload did not contain a tool call")?;
    let live_tool =
        extract_chat_tool_call(&live).context("live chat payload did not contain a tool call")?;

    ensure_eq(
        local["choices"][0]["finish_reason"].as_str(),
        live["choices"][0]["finish_reason"].as_str(),
        "chat tool call finish_reason",
    )?;
    ensure_eq(
        Some(local_tool.name.clone()),
        Some(live_tool.name.clone()),
        "chat tool name",
    )?;
    ensure!(
        value_matches_schema(&local_tool.arguments, &tool_parameters_schema()),
        "local chat tool arguments did not match schema: {}",
        truncate_for_display(&local_tool.arguments.to_string(), 240)
    );
    ensure!(
        value_matches_schema(&live_tool.arguments, &tool_parameters_schema()),
        "live chat tool arguments did not match schema: {}",
        truncate_for_display(&live_tool.arguments.to_string(), 240)
    );

    Ok(())
}

async fn compare_chat_stream_tool_call(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    enqueue_tool_scenario(&server, "chat.completions", &config.model, true).await;

    let request = json!({
        "model": config.model,
        "messages": [{ "role": "user", "content": "Use the weather tool for Boston." }],
        "stream": true,
        "tools": [{
            "type": "function",
            "function": {
                "name": "lookup_weather",
                "description": "Look up the weather",
                "parameters": tool_parameters_schema()
            }
        }],
        "tool_choice": {
            "type": "function",
            "function": {
                "name": "lookup_weather"
            }
        }
    });

    let local = post_sse_ok(&server.api_client(), "/v1/chat/completions", &request).await?;
    let live = post_sse_ok(&config.api, "/v1/chat/completions", &request).await?;

    let local_milestones = normalize_chat_stream(&local)?;
    let live_milestones = normalize_chat_stream(&live)?;

    ensure_eq(
        Some(local_milestones),
        Some(live_milestones),
        "chat tool stream milestones",
    )?;
    ensure_eq(
        extract_chat_stream_tool_name(&local)?,
        extract_chat_stream_tool_name(&live)?,
        "chat tool stream name",
    )?;

    Ok(())
}

async fn compare_chat_image_input(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    let request = json!({
        "model": config.model,
        "stream": false,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": "Describe this image briefly." },
                { "type": "image_url", "image_url": { "url": LIVE_IMAGE_URL } }
            ]
        }]
    });

    let local = post_json_ok(&server.api_client(), "/v1/chat/completions", &request).await?;
    let live = post_json_ok(&config.api, "/v1/chat/completions", &request).await?;

    ensure_eq(
        local["object"].as_str(),
        live["object"].as_str(),
        "chat image object",
    )?;
    ensure!(
        extract_chat_text(&local).is_some(),
        "local chat image payload did not include assistant text: {}",
        truncate_for_display(&local.to_string(), 240)
    );
    ensure!(
        extract_chat_text(&live).is_some(),
        "live chat image payload did not include assistant text: {}",
        truncate_for_display(&live.to_string(), 240)
    );

    Ok(())
}

async fn compare_chat_stream_image_input(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    let request = json!({
        "model": config.model,
        "stream": true,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": "Describe this image briefly." },
                { "type": "image_url", "image_url": { "url": LIVE_IMAGE_URL } }
            ]
        }]
    });

    let local = post_sse_ok(&server.api_client(), "/v1/chat/completions", &request).await?;
    let live = post_sse_ok(&config.api, "/v1/chat/completions", &request).await?;

    ensure_eq(
        Some(normalize_chat_stream(&local)?),
        Some(normalize_chat_stream(&live)?),
        "chat image stream milestones",
    )?;
    ensure!(
        extract_chat_stream_text(&local)?.is_some(),
        "local chat image stream did not include assistant text"
    );
    ensure!(
        extract_chat_stream_text(&live)?.is_some(),
        "live chat image stream did not include assistant text"
    );

    Ok(())
}

async fn compare_chat_tool_choice_none(config: &LiveOptions) -> Result<()> {
    let server = common::spawn_server().await?;
    let request = json!({
        "model": config.model,
        "messages": [{ "role": "user", "content": "Reply with a short greeting without using tools." }],
        "stream": false,
        "tools": [{
            "type": "function",
            "function": {
                "name": "lookup_weather",
                "description": "Look up the weather",
                "parameters": tool_parameters_schema()
            }
        }],
        "tool_choice": "none"
    });

    let local = post_json_ok(&server.api_client(), "/v1/chat/completions", &request).await?;
    let live = post_json_ok(&config.api, "/v1/chat/completions", &request).await?;

    ensure!(
        extract_chat_text(&local).is_some(),
        "local chat tool_choice none did not include assistant text"
    );
    ensure!(
        extract_chat_text(&live).is_some(),
        "live chat tool_choice none did not include assistant text"
    );
    ensure_eq(
        Some(extract_chat_tool_call(&local).is_some()),
        Some(extract_chat_tool_call(&live).is_some()),
        "chat tool_choice none tool call presence",
    )?;
    ensure!(
        extract_chat_tool_call(&local).is_none(),
        "local chat tool_choice none unexpectedly returned tool calls: {}",
        truncate_for_display(&local.to_string(), 240)
    );
    ensure!(
        extract_chat_tool_call(&live).is_none(),
        "live chat tool_choice none unexpectedly returned tool calls: {}",
        truncate_for_display(&live.to_string(), 240)
    );

    Ok(())
}

async fn enqueue_tool_scenario(
    server: &common::TestServer,
    endpoint: &str,
    model: &str,
    stream: bool,
) {
    server
        .enqueue_scenarios(json!({
            "scenarios": [{
                "matcher": {
                    "endpoint": endpoint,
                    "model": model,
                    "stream": stream,
                    "input_contains": "weather tool"
                },
                "script": {
                    "kind": "success",
                    "tool_calls": [{
                        "id": "call_weather",
                        "name": "lookup_weather",
                        "arguments": {
                            "city": "Boston"
                        }
                    }]
                }
            }]
        }))
        .await;
}

async fn enqueue_responses_continuation_scenarios(server: &common::TestServer, model: &str) {
    server
        .enqueue_scenarios(json!({
            "scenarios": [
                {
                    "matcher": {
                        "endpoint": "responses",
                        "model": model,
                        "stream": false,
                        "input_contains": "weather tool"
                    },
                    "script": {
                        "kind": "success",
                        "tool_calls": [{
                            "id": "call_weather",
                            "name": "lookup_weather",
                            "arguments": {
                                "city": "Boston"
                            }
                        }]
                    }
                },
                {
                    "matcher": {
                        "endpoint": "responses",
                        "model": model,
                        "stream": false,
                        "input_contains": "72 and sunny"
                    },
                    "script": {
                        "kind": "success",
                        "response_text": "The weather is 72 and sunny."
                    }
                },
                {
                    "matcher": {
                        "endpoint": "responses",
                        "model": model,
                        "stream": true,
                        "input_contains": "72 and sunny"
                    },
                    "script": {
                        "kind": "success",
                        "response_text": "The weather is 72 and sunny."
                    }
                }
            ]
        }))
        .await;
}

async fn post_json_ok(client: &common::ApiClient, path: &str, body: &Value) -> Result<Value> {
    let response = client.post_json_recorded(path, body).await;
    ensure!(
        response.status == fabro_http::StatusCode::OK,
        "{} returned {}: {}",
        path,
        response.status,
        summarize_recorded_body(&response)
    );
    parse_json_body(&response)
}

async fn post_sse_ok(
    client: &common::ApiClient,
    path: &str,
    body: &Value,
) -> Result<common::ParsedSseTranscript> {
    let response = client.post_json_recorded(path, body).await;
    ensure!(
        response.status == fabro_http::StatusCode::OK,
        "{} returned {}: {}",
        path,
        response.status,
        summarize_recorded_body(&response)
    );
    ensure!(
        response
            .headers
            .get("content-type")
            .is_some_and(|value| value.contains("text/event-stream")),
        "{} did not return text/event-stream: {:?}",
        path,
        response.headers
    );
    common::parse_sse_transcript(&response.body).map_err(|error| anyhow!(error))
}

fn parse_json_body(response: &common::RecordedResponse) -> Result<Value> {
    serde_json::from_slice(&response.body).with_context(|| {
        format!(
            "response body was not valid JSON: {}",
            summarize_recorded_body(response)
        )
    })
}

fn classify_live_access_blocker(response: &common::RecordedResponse) -> Option<String> {
    let body: Value = serde_json::from_slice(&response.body).ok()?;
    let error = body.get("error")?;
    let error_type = error.get("type").and_then(Value::as_str);
    let message = error.get("message").and_then(Value::as_str).unwrap_or("");

    match response.status {
        fabro_http::StatusCode::UNAUTHORIZED | fabro_http::StatusCode::FORBIDDEN
            if message.contains("Missing scopes:")
                || message.contains("insufficient permissions")
                || message.contains("correct role in your organization") =>
        {
            Some("missing required OpenAI API scope or role".to_owned())
        }
        fabro_http::StatusCode::TOO_MANY_REQUESTS if error_type == Some("insufficient_quota") => {
            Some("account has insufficient quota for live API calls".to_owned())
        }
        _ => None,
    }
}

fn ensure_matching_status(
    local: &common::RecordedResponse,
    live: &common::RecordedResponse,
    label: &str,
) -> Result<()> {
    ensure!(
        local.status == live.status,
        "{} status mismatch: local={} body={} live={} body={}",
        label,
        local.status,
        summarize_recorded_body(local),
        live.status,
        summarize_recorded_body(live)
    );
    ensure!(
        local.status == fabro_http::StatusCode::OK,
        "{} returned non-OK responses: local={} body={} live={} body={}",
        label,
        local.status,
        summarize_recorded_body(local),
        live.status,
        summarize_recorded_body(live)
    );

    Ok(())
}

fn normalize_response_stream(transcript: &common::ParsedSseTranscript) -> Vec<String> {
    let mut milestones = Vec::new();

    for event in &transcript.events {
        let Some(name) = event.event.as_deref() else {
            continue;
        };
        if RESPONSES_STREAM_MILESTONES.contains(&name)
            && milestones.last().map(String::as_str) != Some(name)
        {
            milestones.push(name.to_owned());
        }
    }

    milestones
}

fn normalize_chat_stream(
    transcript: &common::ParsedSseTranscript,
) -> Result<Vec<ChatStreamMilestone>> {
    let mut milestones = Vec::new();

    for event in &transcript.events {
        if event.data == "[DONE]" {
            push_chat_milestone(&mut milestones, ChatStreamMilestone::Done);
            continue;
        }

        let payload: Value =
            serde_json::from_str(&event.data).context("chat SSE data was not valid JSON")?;
        let choice = payload
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .ok_or_else(|| anyhow!("chat chunk did not contain choices[0]"))?;
        let delta = choice.get("delta").and_then(Value::as_object);

        if delta
            .and_then(|delta| delta.get("role"))
            .and_then(Value::as_str)
            .is_some()
        {
            push_chat_milestone(&mut milestones, ChatStreamMilestone::Role);
        }
        if delta
            .and_then(|delta| delta.get("content"))
            .and_then(Value::as_str)
            .is_some_and(|content| !content.is_empty())
        {
            push_chat_milestone(&mut milestones, ChatStreamMilestone::ContentDelta);
        }
        if delta
            .and_then(|delta| delta.get("tool_calls"))
            .and_then(Value::as_array)
            .is_some_and(|tool_calls| !tool_calls.is_empty())
        {
            push_chat_milestone(&mut milestones, ChatStreamMilestone::ToolCallDelta);
        }

        match choice.get("finish_reason").and_then(Value::as_str) {
            Some("stop") => push_chat_milestone(&mut milestones, ChatStreamMilestone::FinishStop),
            Some("tool_calls") => {
                push_chat_milestone(&mut milestones, ChatStreamMilestone::FinishToolCalls);
            }
            _ => {}
        }
    }

    Ok(milestones)
}

fn push_chat_milestone(milestones: &mut Vec<ChatStreamMilestone>, milestone: ChatStreamMilestone) {
    if milestones.last() != Some(&milestone) {
        milestones.push(milestone);
    }
}

fn extract_response_text(body: &Value) -> Option<String> {
    body.get("output")?.as_array()?.iter().find_map(|item| {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            return None;
        }

        item.get("content")?.as_array()?.iter().find_map(|content| {
            (content.get("type").and_then(Value::as_str) == Some("output_text"))
                .then(|| content.get("text").and_then(Value::as_str))
                .flatten()
                .map(ToOwned::to_owned)
        })
    })
}

fn extract_response_structured_json(body: &Value) -> Option<Value> {
    for item in body.get("output")?.as_array()? {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }

        for content in item.get("content")?.as_array()? {
            match content.get("type").and_then(Value::as_str) {
                Some("output_json") => {
                    return content.get("json").cloned();
                }
                Some("output_text") => {
                    if let Some(json) = parse_json_object_string(content.get("text")?.as_str()?) {
                        return Some(json);
                    }
                }
                _ => {}
            }
        }
    }

    None
}

fn extract_response_tool_call(body: &Value) -> Option<ToolCallObservation> {
    body.get("output")?
        .as_array()?
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .and_then(|tool_call| {
            Some(ToolCallObservation {
                name:      tool_call.get("name")?.as_str()?.to_owned(),
                arguments: parse_json_object_string(tool_call.get("arguments")?.as_str()?)?,
            })
        })
}

fn extract_response_tool_call_id(body: &Value) -> Option<String> {
    body.get("output")?
        .as_array()?
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))?
        .get("call_id")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn extract_response_stream_structured_json(
    transcript: &common::ParsedSseTranscript,
) -> Result<Option<Value>> {
    Ok(extract_response_stream_text(transcript)?
        .as_deref()
        .and_then(parse_json_object_string))
}

fn extract_response_stream_text(
    transcript: &common::ParsedSseTranscript,
) -> Result<Option<String>> {
    for event in &transcript.events {
        if event.event.as_deref() != Some("response.output_text.done") {
            continue;
        }

        let payload: Value =
            serde_json::from_str(&event.data).context("responses SSE data was not valid JSON")?;
        if let Some(text) = payload.get("text").and_then(Value::as_str) {
            return Ok(Some(text.to_owned()));
        }
    }

    Ok(None)
}

fn extract_response_stream_tool_call(
    transcript: &common::ParsedSseTranscript,
) -> Result<Option<ToolCallObservation>> {
    for event in &transcript.events {
        if event.event.as_deref() != Some("response.output_item.done") {
            continue;
        }

        let payload: Value =
            serde_json::from_str(&event.data).context("responses SSE data was not valid JSON")?;
        let Some(item) = payload.get("item") else {
            continue;
        };
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            continue;
        }

        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(arguments) = item.get("arguments").and_then(Value::as_str) else {
            continue;
        };
        let Some(arguments) = parse_json_object_string(arguments) else {
            continue;
        };

        return Ok(Some(ToolCallObservation {
            name: name.to_owned(),
            arguments,
        }));
    }

    Ok(None)
}

fn extract_chat_text(body: &Value) -> Option<String> {
    body.get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn extract_chat_structured_json(body: &Value) -> Option<Value> {
    parse_json_object_string(&extract_chat_text(body)?)
}

fn extract_chat_tool_call(body: &Value) -> Option<ToolCallObservation> {
    body.get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("tool_calls")?
        .as_array()?
        .iter()
        .find_map(|tool_call| {
            Some(ToolCallObservation {
                name:      tool_call.get("function")?.get("name")?.as_str()?.to_owned(),
                arguments: parse_json_object_string(
                    tool_call.get("function")?.get("arguments")?.as_str()?,
                )?,
            })
        })
}

fn extract_chat_stream_tool_name(
    transcript: &common::ParsedSseTranscript,
) -> Result<Option<String>> {
    for event in &transcript.events {
        if event.data == "[DONE]" {
            continue;
        }

        let payload: Value =
            serde_json::from_str(&event.data).context("chat SSE data was not valid JSON")?;
        let Some(tool_calls) = payload
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("tool_calls"))
            .and_then(Value::as_array)
        else {
            continue;
        };

        for tool_call in tool_calls {
            if let Some(name) = tool_call
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
            {
                return Ok(Some(name.to_owned()));
            }
        }
    }

    Ok(None)
}

fn extract_chat_stream_text(transcript: &common::ParsedSseTranscript) -> Result<Option<String>> {
    let mut content = String::new();

    for event in &transcript.events {
        if event.data == "[DONE]" {
            continue;
        }

        let payload: Value =
            serde_json::from_str(&event.data).context("chat SSE data was not valid JSON")?;
        if let Some(delta) = payload
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("content"))
            .and_then(Value::as_str)
        {
            content.push_str(delta);
        }
    }

    if content.is_empty() {
        Ok(None)
    } else {
        Ok(Some(content))
    }
}

fn extract_chat_stream_structured_json(
    transcript: &common::ParsedSseTranscript,
) -> Result<Option<Value>> {
    Ok(extract_chat_stream_text(transcript)?
        .as_deref()
        .and_then(parse_json_object_string))
}

fn has_responses_usage(body: &Value) -> bool {
    body.get("usage")
        .and_then(Value::as_object)
        .is_some_and(|usage| {
            usage.get("input_tokens").and_then(Value::as_u64).is_some()
                && usage.get("output_tokens").and_then(Value::as_u64).is_some()
                && usage.get("total_tokens").and_then(Value::as_u64).is_some()
        })
}

fn has_chat_usage(body: &Value) -> bool {
    body.get("usage")
        .and_then(Value::as_object)
        .is_some_and(|usage| {
            usage.get("prompt_tokens").and_then(Value::as_u64).is_some()
                && usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .is_some()
                && usage.get("total_tokens").and_then(Value::as_u64).is_some()
        })
}

fn smoke_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "message": { "type": "string" },
            "ok": { "type": "boolean" }
        },
        "required": ["message", "ok"],
        "additionalProperties": false
    })
}

fn tool_parameters_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "city": { "type": "string" }
        },
        "required": ["city"],
        "additionalProperties": false
    })
}

fn value_matches_schema(value: &Value, schema: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return false;
    };
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let allow_additional = schema
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    for key in required.iter().filter_map(Value::as_str) {
        if !object.contains_key(key) {
            return false;
        }
    }

    if !allow_additional && object.keys().any(|key| !properties.contains_key(key)) {
        return false;
    }

    object.iter().all(|(key, value)| {
        let Some(property_schema) = properties.get(key) else {
            return allow_additional;
        };

        match property_schema.get("type").and_then(Value::as_str) {
            Some("string") => value.is_string(),
            Some("boolean") => value.is_boolean(),
            Some("integer") => value.as_i64().is_some(),
            Some("number") => value.as_f64().is_some(),
            Some("object") => value_matches_schema(value, property_schema),
            _ => false,
        }
    })
}

fn parse_json_object_string(input: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(input).ok()?;
    value.is_object().then_some(value)
}

fn summarize_recorded_body(response: &common::RecordedResponse) -> String {
    truncate_for_display(&String::from_utf8_lossy(&response.body), 240)
}

fn truncate_for_display(input: &str, max_chars: usize) -> String {
    let collapsed = input.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();

    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "This helper mirrors the owned values compared by the tests."
)]
fn ensure_eq<T>(left: Option<T>, right: Option<T>, label: &str) -> Result<()>
where
    T: PartialEq + std::fmt::Debug,
{
    ensure!(
        left == right,
        "{label} mismatch: local={left:?} live={right:?}"
    );
    Ok(())
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
