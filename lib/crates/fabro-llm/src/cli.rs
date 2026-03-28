use std::io::{self, IsTerminal, Read, Write};

use dialoguer::console::Term;
use dialoguer::theme::ColorfulTheme;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use cli_table::format::{Border, Justify, Separator};
use cli_table::{Cell, CellStruct, Color, Style, Table, print_stdout};
use futures::{StreamExt, stream};
use serde::Deserialize;
use tokio::task;
use tokio::time;

use fabro_util::terminal::Styles;

use fabro_model::{Catalog, Model, Provider};

use crate::generate::{self, GenerateParams};
use crate::tools::Tool;
use crate::types::{ContentPart, GenerateResult, Message, ReasoningEffort, StreamEvent, Usage};

pub struct ServerConnection {
    pub client: reqwest::Client,
    pub base_url: String,
}

#[derive(Args)]
pub struct PromptArgs {
    /// The prompt text (also accepts stdin)
    pub prompt: Option<String>,

    /// Model to use
    #[arg(short, long)]
    pub model: Option<String>,

    /// System prompt
    #[arg(short, long)]
    pub system: Option<String>,

    /// Do not stream output
    #[arg(long)]
    pub no_stream: bool,

    /// Show token usage
    #[arg(short, long)]
    pub usage: bool,

    /// JSON schema for structured output (inline JSON string)
    #[arg(short = 'S', long)]
    pub schema: Option<String>,

    /// key=value options (temperature, `max_tokens`, `top_p`)
    #[arg(short, long, value_parser = parse_option)]
    pub option: Vec<(String, String)>,
}

#[derive(Subcommand)]
pub enum ModelsCommand {
    /// List available models
    List {
        /// Filter by provider
        #[arg(short, long)]
        provider: Option<String>,

        /// Search for models matching this string
        #[arg(short, long)]
        query: Option<String>,
    },

    /// Test model availability by sending a simple prompt
    Test {
        /// Filter by provider
        #[arg(short, long)]
        provider: Option<String>,

        /// Test a specific model
        #[arg(short, long)]
        model: Option<String>,

        /// Run a multi-turn tool-use test (catches reasoning round-trip bugs)
        #[arg(long)]
        deep: bool,
    },
}

fn parse_option(s: &str) -> Result<(String, String), String> {
    let (key, value) = s
        .split_once('=')
        .ok_or_else(|| format!("expected key=value, got {s}"))?;
    Ok((key.to_string(), value.to_string()))
}

fn format_context_window(tokens: i64) -> String {
    let rounded = ((tokens + 500) / 1_000) * 1_000;
    if rounded >= 1_000_000 {
        format!("{}m", rounded / 1_000_000)
    } else if rounded >= 1_000 {
        format!("{}k", rounded / 1_000)
    } else {
        tokens.to_string()
    }
}

fn format_cost(cost: Option<f64>) -> String {
    match cost {
        None => "-".to_string(),
        Some(c) => format!("${c:.1}"),
    }
}

fn format_speed(tps: Option<f64>) -> String {
    match tps {
        None => "-".to_string(),
        Some(t) => format!("{} tok/s", t as i64),
    }
}

fn color_if(use_color: bool, color: Color) -> Option<Color> {
    if use_color { Some(color) } else { None }
}

fn model_row(model: &Model, use_color: bool) -> Vec<CellStruct> {
    let aliases = model.aliases.join(", ");
    let cost = format!(
        "{} / {}",
        format_cost(model.costs.input_cost_per_mtok),
        format_cost(model.costs.output_cost_per_mtok),
    );
    vec![
        model.id.clone().cell().bold(use_color),
        model
            .provider
            .cell()
            .foreground_color(color_if(use_color, Color::Ansi256(8))),
        aliases
            .cell()
            .foreground_color(color_if(use_color, Color::Ansi256(8))),
        format_context_window(model.limits.context_window)
            .cell()
            .justify(Justify::Right),
        cost.cell().justify(Justify::Right),
        format_speed(model.estimated_output_tps)
            .cell()
            .justify(Justify::Right)
            .foreground_color(color_if(use_color, Color::Cyan)),
    ]
}

fn models_title() -> Vec<CellStruct> {
    vec![
        "MODEL".cell().bold(true),
        "PROVIDER".cell().bold(true),
        "ALIASES".cell().bold(true),
        "CONTEXT".cell().bold(true).justify(Justify::Right),
        "COST".cell().bold(true).justify(Justify::Right),
        "SPEED".cell().bold(true).justify(Justify::Right),
    ]
}

fn print_models_table(models: &[Model], s: &Styles) {
    let use_color = s.use_color;
    let rows: Vec<Vec<CellStruct>> = models.iter().map(|m| model_row(m, use_color)).collect();
    let table = rows
        .table()
        .title(models_title())
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    let _ = print_stdout(table);
}

fn read_stdin_prompt() -> Option<String> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut buf = String::new();
    stdin.lock().read_to_string(&mut buf).ok()?;
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn resolve_prompt(arg: Option<String>, stdin: Option<String>) -> Result<String> {
    match (stdin, arg) {
        (Some(s), Some(a)) => Ok(format!("{s}\n{a}")),
        (Some(s), None) => Ok(s),
        (None, Some(a)) => Ok(a),
        (None, None) => {
            bail!("Error: no prompt provided. Pass a prompt as an argument or pipe text via stdin.")
        }
    }
}

/// Returns (`model_id`, provider) from the catalog, falling back to the first catalog model.
fn resolve_model(model_arg: Option<String>) -> (String, Option<String>) {
    let raw = model_arg.unwrap_or_else(|| {
        Catalog::builtin()
            .list(None)
            .first()
            .map_or_else(|| "claude-sonnet-4-5".to_string(), |m| m.id.clone())
    });
    match Catalog::builtin().get(&raw) {
        Some(info) => (info.id.clone(), Some(info.provider.to_string())),
        None => (raw, None),
    }
}

fn apply_options(
    mut params: GenerateParams,
    options: &[(String, String)],
) -> Result<GenerateParams> {
    let mut provider_opts = serde_json::Map::new();

    for (key, value) in options {
        match key.as_str() {
            "temperature" => {
                let v: f64 = value
                    .parse()
                    .with_context(|| format!("invalid temperature value: {value}"))?;
                params = params.temperature(v);
            }
            "max_tokens" => {
                let v: i64 = value
                    .parse()
                    .with_context(|| format!("invalid max_tokens value: {value}"))?;
                params = params.max_tokens(v);
            }
            "top_p" => {
                let v: f64 = value
                    .parse()
                    .with_context(|| format!("invalid top_p value: {value}"))?;
                params = params.top_p(v);
            }
            _ => {
                provider_opts.insert(key.clone(), serde_json::Value::String(value.clone()));
            }
        }
    }

    if !provider_opts.is_empty() {
        params = params.provider_options(serde_json::Value::Object(provider_opts));
    }

    Ok(params)
}

fn print_usage(usage: &Usage) {
    eprintln!(
        "Tokens: {} input, {} output, {} total",
        usage.input_tokens, usage.output_tokens, usage.total_tokens
    );
}

#[derive(Args)]
pub struct ChatArgs {
    /// Model to use
    #[arg(short, long)]
    pub model: Option<String>,

    /// System prompt
    #[arg(short, long)]
    pub system: Option<String>,
}

pub async fn run_chat(args: ChatArgs) -> Result<()> {
    let (model_id, provider) = resolve_model(args.model);
    eprintln!("Using model: {model_id}");

    let mut messages: Vec<Message> = Vec::new();
    let is_tty = io::stdin().is_terminal();

    loop {
        let line = if is_tty {
            let result = task::spawn_blocking(|| {
                dialoguer::Input::<String>::with_theme(&ColorfulTheme::default())
                    .with_prompt(">")
                    .interact_on(&Term::stderr())
            })
            .await?;
            match result {
                Ok(line) => line,
                Err(_) => break,
            }
        } else {
            eprint!("> ");
            io::stderr().flush()?;
            let mut buf = String::new();
            if io::stdin().read_line(&mut buf)? == 0 {
                break;
            }
            buf.trim_end().to_string()
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        messages.push(Message::user(trimmed));

        let mut params = GenerateParams::new(&model_id)
            .messages(messages.clone())
            .max_tokens(4096);
        if let Some(ref p) = provider {
            params = params.provider(p);
        }
        if let Some(ref sys) = args.system {
            params = params.system(sys);
        }

        let mut stream_result = generate::stream(params).await?;
        let mut full_text = String::new();
        while let Some(event) = stream_result.next().await {
            if let StreamEvent::TextDelta { delta, .. } = event? {
                print!("{delta}");
                full_text.push_str(&delta);
            }
        }
        println!();

        messages.push(Message::assistant(full_text));
    }

    Ok(())
}

pub async fn run_prompt(args: PromptArgs) -> Result<()> {
    let stdin_prompt = read_stdin_prompt();
    let prompt_text = resolve_prompt(args.prompt, stdin_prompt)?;
    let (model_id, provider) = resolve_model(args.model);

    eprintln!("Using model: {model_id}");

    let mut params = GenerateParams::new(&model_id).prompt(&prompt_text);
    if let Some(p) = provider {
        params = params.provider(&p);
    }
    if let Some(sys) = args.system {
        params = params.system(&sys);
    }
    params = apply_options(params, &args.option)?;

    let schema: Option<serde_json::Value> = match &args.schema {
        Some(s) => Some(serde_json::from_str(s).context("--schema must be valid JSON")?),
        None => None,
    };

    match (args.no_stream, schema) {
        (true, Some(schema)) => {
            let result = generate::generate_object(params, schema).await?;
            let object = result.output.as_ref().unwrap_or(&serde_json::Value::Null);
            println!("{}", serde_json::to_string_pretty(object)?);
            if args.usage {
                print_usage(&result.usage);
            }
        }
        (true, None) => {
            let result = generate::generate(params).await?;
            print!("{}", result.text());
            if args.usage {
                print_usage(&result.usage);
            }
        }
        (false, Some(schema)) => {
            let mut stream_result = generate::stream_object(params, schema).await?;
            while let Some(event) = stream_result.next().await {
                event?;
            }
            if let Some(object) = stream_result.object() {
                println!("{}", serde_json::to_string_pretty(object)?);
            }
        }
        (false, None) => {
            let mut stream_result = generate::stream(params).await?;
            while let Some(event) = stream_result.next().await {
                if let StreamEvent::TextDelta { delta, .. } = event? {
                    print!("{delta}");
                }
            }
            println!();
            if args.usage {
                if let Some(response) = stream_result.response() {
                    print_usage(&response.usage);
                }
            }
        }
    }

    Ok(())
}

pub async fn run_prompt_via_server(args: PromptArgs, server: &ServerConnection) -> Result<()> {
    let stdin_prompt = read_stdin_prompt();
    let prompt_text = resolve_prompt(args.prompt, stdin_prompt)?;

    // Extract known options
    let mut temperature: Option<f64> = None;
    let mut max_tokens: Option<i64> = None;
    let mut top_p: Option<f64> = None;
    for (key, value) in &args.option {
        match key.as_str() {
            "temperature" => {
                temperature = Some(
                    value
                        .parse()
                        .with_context(|| format!("invalid temperature value: {value}"))?,
                );
            }
            "max_tokens" => {
                max_tokens = Some(
                    value
                        .parse()
                        .with_context(|| format!("invalid max_tokens value: {value}"))?,
                );
            }
            "top_p" => {
                top_p = Some(
                    value
                        .parse()
                        .with_context(|| format!("invalid top_p value: {value}"))?,
                );
            }
            _ => {}
        }
    }

    let schema: Option<serde_json::Value> = match &args.schema {
        Some(s) => Some(serde_json::from_str(s).context("--schema must be valid JSON")?),
        None => None,
    };

    // Force non-streaming for structured output
    let use_stream = !args.no_stream && schema.is_none();

    let mut body = serde_json::json!({
        "messages": [{"role": "user", "content": [{"kind": "text", "data": prompt_text}]}],
        "stream": use_stream,
    });
    if let Some(ref model) = args.model {
        body["model"] = serde_json::Value::String(model.clone());
    }
    if let Some(ref system) = args.system {
        body["system"] = serde_json::Value::String(system.clone());
    }
    if let Some(ref schema) = schema {
        body["schema"] = schema.clone();
    }
    if let Some(t) = temperature {
        body["temperature"] = serde_json::json!(t);
    }
    if let Some(m) = max_tokens {
        body["max_tokens"] = serde_json::json!(m);
    }
    if let Some(t) = top_p {
        body["top_p"] = serde_json::json!(t);
    }

    let url = format!("{}/completions", server.base_url);

    if use_stream {
        let response = server
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("Failed to connect to server at {}", server.base_url))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Server returned {status}: {text}");
        }

        let show_usage = args.usage;
        let mut output_usage: Option<Usage> = None;

        parse_sse_frames(response, |event_type, data| {
            if event_type == "stream_event" {
                if let Ok(event) = serde_json::from_str::<StreamEvent>(data) {
                    match event {
                        StreamEvent::TextDelta { delta, .. } => {
                            print!("{delta}");
                            let _ = io::stdout().flush();
                        }
                        StreamEvent::Finish { usage, .. } => {
                            if show_usage {
                                output_usage = Some(usage);
                            }
                        }
                        StreamEvent::Error { error, .. } => {
                            bail!("Server error: {error}");
                        }
                        _ => {}
                    }
                }
            }
            Ok(true)
        })
        .await?;
        println!();

        if show_usage {
            if let Some(usage) = output_usage {
                print_usage(&usage);
            }
        }
    } else {
        // Non-streaming
        let response = server
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("Failed to connect to server at {}", server.base_url))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Server returned {status}: {text}");
        }

        let result: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse completion response")?;

        if schema.is_some() {
            if let Some(output) = result.get("output") {
                println!("{}", serde_json::to_string_pretty(output)?);
            } else {
                // Extract text from message.content parts
                print_message_text(&result["message"]);
            }
        } else {
            print_message_text(&result["message"]);
        }

        if args.usage {
            let input = result["usage"]["input_tokens"].as_i64().unwrap_or(0);
            let output = result["usage"]["output_tokens"].as_i64().unwrap_or(0);
            eprintln!(
                "Tokens: {} input, {} output, {} total",
                input,
                output,
                input + output
            );
        }
    }

    Ok(())
}

/// Extract and print text from a CompletionMessage JSON value.
fn print_message_text(message: &serde_json::Value) {
    if let Some(content) = message["content"].as_array() {
        for part in content {
            if part["kind"].as_str() == Some("text") {
                if let Some(text) = part["data"].as_str() {
                    print!("{text}");
                }
            }
        }
    }
}

/// Parse SSE frames from a server response, calling `on_frame` for each complete frame.
///
/// Each frame provides `(event_type, data)`.
async fn parse_sse_frames(
    response: reqwest::Response,
    mut on_frame: impl FnMut(&str, &str) -> Result<bool>,
) -> Result<()> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Error reading stream")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = buffer.find("\n\n") {
            let frame = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();

            let mut event_type = String::new();
            let mut data = String::new();
            for line in frame.lines() {
                if let Some(val) = line.strip_prefix("event: ") {
                    event_type = val.to_string();
                } else if let Some(val) = line.strip_prefix("data: ") {
                    data = val.to_string();
                }
            }

            if !on_frame(&event_type, &data)? {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Stream session SSE events, printing text deltas to stdout in real-time.
async fn stream_session_text(response: reqwest::Response) -> Result<()> {
    parse_sse_frames(response, |event_type, data| {
        match event_type {
            "content_delta" => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(delta) = parsed["delta"].as_str() {
                        print!("{delta}");
                        let _ = io::stdout().flush();
                    }
                }
            }
            "assistant_turn" => {
                // Text already printed via content_delta events
            }
            "done" => {
                println!();
                return Ok(false);
            }
            "error" => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
                    let msg = parsed["message"].as_str().unwrap_or("Unknown error");
                    bail!("Server error: {msg}");
                }
            }
            _ => {}
        }
        Ok(true)
    })
    .await
}

pub async fn run_chat_via_server(args: ChatArgs, server: &ServerConnection) -> Result<()> {
    let is_tty = io::stdin().is_terminal();
    let mut session_id: Option<String> = None;

    loop {
        let line = if is_tty {
            let result = task::spawn_blocking(|| {
                dialoguer::Input::<String>::with_theme(&ColorfulTheme::default())
                    .with_prompt(">")
                    .interact_on(&Term::stderr())
            })
            .await?;
            match result {
                Ok(line) => line,
                Err(_) => break,
            }
        } else {
            eprint!("> ");
            io::stderr().flush()?;
            let mut buf = String::new();
            if io::stdin().read_line(&mut buf)? == 0 {
                break;
            }
            buf.trim_end().to_string()
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(sid) = &session_id {
            // Subsequent messages: send message
            let body = serde_json::json!({ "content": trimmed });
            let url = format!("{}/sessions/{sid}/messages", server.base_url);
            let response = server
                .client
                .post(&url)
                .json(&body)
                .send()
                .await
                .with_context(|| format!("Failed to connect to server at {}", server.base_url))?;

            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                bail!("Server returned {status}: {text}");
            }

            // Stream events
            let events_url = format!("{}/sessions/{sid}/events", server.base_url);
            let events_response = server
                .client
                .get(&events_url)
                .send()
                .await
                .context("Failed to connect to event stream")?;

            if !events_response.status().is_success() {
                let text = events_response.text().await.unwrap_or_default();
                bail!("Event stream returned error: {text}");
            }

            stream_session_text(events_response).await?;
        } else {
            // First message: create session
            let mut body = serde_json::json!({ "content": trimmed });
            if let Some(ref model) = args.model {
                body["model"] = serde_json::Value::String(model.clone());
            }
            if let Some(ref system) = args.system {
                body["system"] = serde_json::Value::String(system.clone());
            }

            let url = format!("{}/sessions", server.base_url);
            let response = server
                .client
                .post(&url)
                .json(&body)
                .send()
                .await
                .with_context(|| format!("Failed to connect to server at {}", server.base_url))?;

            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                bail!("Server returned {status}: {text}");
            }

            let create_resp: serde_json::Value = response
                .json()
                .await
                .context("Failed to parse session creation response")?;

            let sid = create_resp["id"]
                .as_str()
                .context("Missing session id in response")?
                .to_string();
            let model_id = create_resp["model"]["id"].as_str().unwrap_or("unknown");
            eprintln!("Using model: {model_id}");

            // Stream events
            let events_url = format!("{}/sessions/{sid}/events", server.base_url);
            let events_response = server
                .client
                .get(&events_url)
                .send()
                .await
                .context("Failed to connect to event stream")?;

            if !events_response.status().is_success() {
                let text = events_response.text().await.unwrap_or_default();
                bail!("Event stream returned error: {text}");
            }

            stream_session_text(events_response).await?;
            session_id = Some(sid);
        }
    }

    Ok(())
}

#[derive(Deserialize)]
struct PaginatedModelsResponse {
    data: Vec<Model>,
}

#[derive(Deserialize)]
struct ModelTestResponse {
    status: String,
    error_message: Option<String>,
}

async fn fetch_models_from_server(
    client: &reqwest::Client,
    base_url: &str,
    provider: Option<&str>,
) -> Result<Vec<Model>> {
    let url = format!("{base_url}/models?page[limit]=100");
    tracing::debug!(url = %url, "Fetching models from server");

    let response = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("Failed to connect to server at {base_url}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("Server returned {status}: {body}");
    }

    let parsed: PaginatedModelsResponse = response
        .json()
        .await
        .context("Failed to parse models response from server")?;

    let mut models = parsed.data;
    tracing::debug!(model_count = models.len(), "Models received from server");

    if let Some(p) = provider {
        models.retain(|m| m.provider.as_str() == p);
    }

    Ok(models)
}

async fn test_model_via_server(
    client: &reqwest::Client,
    base_url: &str,
    model_id: &str,
) -> Result<ModelTestResponse> {
    let url = format!("{base_url}/models/{model_id}/test");
    let response = client
        .post(&url)
        .send()
        .await
        .with_context(|| format!("Failed to connect to server at {base_url}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("Server returned {status}: {body}");
    }

    response
        .json()
        .await
        .context("Failed to parse model test response from server")
}

fn build_deep_test_params(info: &Model) -> Option<GenerateParams> {
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
            let a = args.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
            let b = args.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(serde_json::json!(a + b))
        },
    );

    let mut params = GenerateParams::new(&info.id)
        .provider(info.provider.as_str())
        .prompt(
            "I have three numbers: 15, 27, and 42. \
             First use the add tool to compute 15 + 27, \
             then use the add tool to add that result to 42. \
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

fn validate_deep_result(result: &GenerateResult, info: &Model) -> (cli_table::Color, String) {
    // Check tool use: need at least 2 steps (tool call + follow-up)
    if result.steps.len() < 2 {
        return (
            Color::Red,
            "deep: fail (model did not call tool)".to_string(),
        );
    }

    // Check that step 0 had tool results (tool was executed)
    if result.steps[0].tool_results.is_empty() {
        return (Color::Red, "deep: fail (tool not executed)".to_string());
    }

    // Check correctness: 15+27=42, 42+42=84 — final response should contain "84"
    let final_text = result.response.text();
    if !final_text.contains("84") {
        return (Color::Red, "deep: fail (wrong answer)".to_string());
    }

    // Check reasoning if the model supports it
    if info.features.reasoning {
        let has_reasoning = result.steps.iter().any(|step| {
            step.response.message.content.iter().any(|part| {
                matches!(part, ContentPart::Thinking(_))
                    || matches!(part, ContentPart::Other { kind, .. } if kind == ContentPart::OPENAI_REASONING)
            })
        });
        if !has_reasoning {
            return (Color::Yellow, "deep: ok (no reasoning)".to_string());
        }
    }

    (Color::Green, "deep: ok".to_string())
}

async fn test_models_via_server(
    server: &ServerConnection,
    provider: Option<&str>,
    model: Option<&str>,
    deep: bool,
    s: &Styles,
) -> Result<()> {
    if deep {
        eprintln!("Warning: --deep is not supported in server mode");
    }
    let models_to_test = if let Some(model_id) = model {
        let all = fetch_models_from_server(&server.client, &server.base_url, None).await?;
        let found: Vec<_> = all.into_iter().filter(|m| m.id == model_id).collect();
        if found.is_empty() {
            bail!("Unknown model: {model_id}");
        }
        found
    } else {
        fetch_models_from_server(&server.client, &server.base_url, provider).await?
    };

    if models_to_test.is_empty() {
        bail!("No models found");
    }

    let use_color = s.use_color;
    let mut title = models_title();
    title.push("RESULT".cell().bold(true));

    let mut rows: Vec<Vec<CellStruct>> = Vec::new();
    let mut failures = 0u32;
    for info in &models_to_test {
        eprint!("Testing {}...", info.id);
        let result = test_model_via_server(&server.client, &server.base_url, &info.id).await;
        eprintln!(" done");

        let (result_color, status) = match result {
            Ok(resp) if resp.status == "ok" => (Color::Green, "ok".to_string()),
            Ok(resp) => {
                failures += 1;
                let msg = resp
                    .error_message
                    .unwrap_or_else(|| "unknown error".to_string());
                (Color::Red, format!("error: {msg}"))
            }
            Err(e) => {
                failures += 1;
                (Color::Red, format!("error: {e}"))
            }
        };

        let mut row = model_row(info, use_color);
        row.push(
            status
                .cell()
                .foreground_color(color_if(use_color, result_color)),
        );
        rows.push(row);
    }

    let table = rows
        .table()
        .title(title)
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    print_stdout(table)?;

    if failures > 0 {
        bail!("{failures} model(s) failed");
    }

    Ok(())
}

pub async fn run_models(
    command: Option<ModelsCommand>,
    server: Option<ServerConnection>,
) -> Result<()> {
    let command = command.unwrap_or(ModelsCommand::List {
        provider: None,
        query: None,
    });

    let styles = Styles::detect_stdout();

    match command {
        ModelsCommand::List { provider, query } => {
            let mut models = match &server {
                Some(s) => {
                    fetch_models_from_server(&s.client, &s.base_url, provider.as_deref()).await?
                }
                None => {
                    let p = provider.as_deref().and_then(|s| s.parse::<Provider>().ok());
                    Catalog::builtin().list(p).into_iter().cloned().collect()
                }
            };

            if let Some(q) = &query {
                let q_lower = q.to_lowercase();
                models.retain(|m| {
                    m.id.to_lowercase().contains(&q_lower)
                        || m.display_name.to_lowercase().contains(&q_lower)
                        || m.aliases
                            .iter()
                            .any(|a| a.to_lowercase().contains(&q_lower))
                });
            }

            print_models_table(&models, &styles);
        }
        ModelsCommand::Test {
            provider,
            model,
            deep,
        } => match &server {
            Some(s) => {
                test_models_via_server(s, provider.as_deref(), model.as_deref(), deep, &styles)
                    .await?;
            }
            None => {
                test_models(provider.as_deref(), model.as_deref(), deep, &styles).await?;
            }
        },
    }

    Ok(())
}

async fn test_one_model(info: &Model, deep: bool) -> (Color, String) {
    if deep {
        match build_deep_test_params(info) {
            None => (Color::Yellow, "deep: skipped (no tool support)".to_string()),
            Some(params) => {
                let result =
                    time::timeout(Duration::from_secs(90), generate::generate(params)).await;
                match result {
                    Ok(Ok(ref gen_result)) => validate_deep_result(gen_result, info),
                    Ok(Err(e)) => (Color::Red, format!("deep: error: {e}")),
                    Err(_) => (Color::Red, "deep: error: timeout (90s)".to_string()),
                }
            }
        }
    } else {
        let params = GenerateParams::new(&info.id)
            .provider(info.provider.as_str())
            .prompt("Say OK")
            .max_tokens(16);

        let result = time::timeout(Duration::from_secs(30), generate::generate(params)).await;
        match result {
            Ok(Ok(_)) => (Color::Green, "ok".to_string()),
            Ok(Err(e)) => (Color::Red, format!("error: {e}")),
            Err(_) => (Color::Red, "error: timeout (30s)".to_string()),
        }
    }
}

async fn test_models(
    provider: Option<&str>,
    model: Option<&str>,
    deep: bool,
    s: &Styles,
) -> Result<()> {
    use rand::seq::SliceRandom;

    let models_to_test = if let Some(model_id) = model {
        match Catalog::builtin().get(model_id) {
            Some(info) => vec![info.clone()],
            None => bail!("Unknown model: {model_id}"),
        }
    } else {
        let p = provider.and_then(|s| s.parse::<Provider>().ok());
        Catalog::builtin().list(p).into_iter().cloned().collect()
    };

    if models_to_test.is_empty() {
        bail!("No models found");
    }

    let test_kind = if deep { "Deep testing" } else { "Testing" };
    let pb = indicatif::ProgressBar::new(models_to_test.len() as u64);
    pb.set_style(
        indicatif::ProgressStyle::with_template(&format!(
            "{{spinner:.green}} {test_kind} {{pos}}/{{len}} models {{wide_bar}} {{eta}}"
        ))
        .unwrap(),
    );
    pb.enable_steady_tick(Duration::from_millis(100));

    // Build (original_index, model_info) pairs, then shuffle for provider spread
    let mut indexed: Vec<(usize, &Model)> = models_to_test.iter().enumerate().collect();
    indexed.shuffle(&mut rand::thread_rng());

    // Run tests concurrently, 6 at a time
    let results: Vec<(usize, Color, String)> = stream::iter(indexed)
        .map(|(idx, info)| {
            let pb = pb.clone();
            async move {
                let (color, status) = test_one_model(info, deep).await;
                pb.inc(1);
                (idx, color, status)
            }
        })
        .buffer_unordered(6)
        .collect()
        .await;

    pb.finish_and_clear();

    // Sort back to original catalog order
    let mut sorted_results = results;
    sorted_results.sort_by_key(|(idx, _, _)| *idx);

    let use_color = s.use_color;
    let mut title = models_title();
    title.push("RESULT".cell().bold(true));

    let mut rows: Vec<Vec<CellStruct>> = Vec::new();
    let mut failures = 0u32;
    for (idx, result_color, status) in &sorted_results {
        if *result_color == Color::Red {
            failures += 1;
        }
        let mut row = model_row(&models_to_test[*idx], use_color);
        row.push(
            status
                .cell()
                .foreground_color(color_if(use_color, *result_color)),
        );
        rows.push(row);
    }

    let table = rows
        .table()
        .title(title)
        .border(Border::builder().build())
        .separator(Separator::builder().build());
    print_stdout(table)?;

    if failures > 0 {
        bail!("{failures} model(s) failed");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_option ---

    #[test]
    fn parse_option_valid() {
        let (k, v) = parse_option("temperature=0.7").unwrap();
        assert_eq!(k, "temperature");
        assert_eq!(v, "0.7");
    }

    #[test]
    fn parse_option_value_with_equals() {
        let (k, v) = parse_option("key=a=b").unwrap();
        assert_eq!(k, "key");
        assert_eq!(v, "a=b");
    }

    #[test]
    fn parse_option_no_equals() {
        assert!(parse_option("nope").is_err());
    }

    // --- format_context_window ---

    #[test]
    fn format_context_window_millions() {
        assert_eq!(format_context_window(1_000_000), "1m");
    }

    #[test]
    fn format_context_window_thousands() {
        assert_eq!(format_context_window(128_000), "128k");
    }

    #[test]
    fn format_context_window_small() {
        assert_eq!(format_context_window(400), "400");
    }

    #[test]
    fn format_context_window_rounds_up() {
        // 1500 rounds to 2000 -> "2k"
        assert_eq!(format_context_window(1500), "2k");
    }

    #[test]
    fn format_context_window_rounds_down() {
        // 1499 rounds to 1000 -> "1k"
        assert_eq!(format_context_window(1499), "1k");
    }

    #[test]
    fn format_context_window_zero() {
        assert_eq!(format_context_window(0), "0");
    }

    // --- format_cost ---

    #[test]
    fn format_cost_none() {
        assert_eq!(format_cost(None), "-");
    }

    #[test]
    fn format_cost_some() {
        assert_eq!(format_cost(Some(3.0)), "$3.0");
    }

    #[test]
    fn format_cost_fractional() {
        assert_eq!(format_cost(Some(15.75)), "$15.8");
    }

    // --- format_speed ---

    #[test]
    fn format_speed_none() {
        assert_eq!(format_speed(None), "-");
    }

    #[test]
    fn format_speed_some() {
        assert_eq!(format_speed(Some(85.5)), "85 tok/s");
    }

    // --- resolve_prompt ---

    #[test]
    fn resolve_prompt_arg_only() {
        let result = resolve_prompt(Some("hello".into()), None).unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn resolve_prompt_stdin_only() {
        let result = resolve_prompt(None, Some("piped".into())).unwrap();
        assert_eq!(result, "piped");
    }

    #[test]
    fn resolve_prompt_both_concatenates() {
        let result = resolve_prompt(Some("arg".into()), Some("stdin".into())).unwrap();
        assert_eq!(result, "stdin\narg");
    }

    #[test]
    fn resolve_prompt_neither_errors() {
        assert!(resolve_prompt(None, None).is_err());
    }

    // --- resolve_model ---

    #[test]
    fn resolve_model_explicit_known() {
        let (model, provider) = resolve_model(Some("claude-sonnet-4-5".into()));
        assert_eq!(model, "claude-sonnet-4-5");
        assert_eq!(provider, Some("anthropic".to_string()));
    }

    #[test]
    fn resolve_model_explicit_unknown() {
        let (model, provider) = resolve_model(Some("nonexistent-model-xyz".into()));
        assert_eq!(model, "nonexistent-model-xyz");
        assert_eq!(provider, None);
    }

    #[test]
    fn resolve_model_none_uses_default() {
        let (model, provider) = resolve_model(None);
        // Should return some valid model from catalog
        assert!(!model.is_empty());
        assert!(provider.is_some());
    }

    // --- apply_options ---

    #[test]
    fn apply_options_temperature() {
        let params = GenerateParams::new("test-model");
        let result = apply_options(params, &[("temperature".into(), "0.7".into())]).unwrap();
        assert_eq!(result.temperature, Some(0.7));
    }

    #[test]
    fn apply_options_max_tokens() {
        let params = GenerateParams::new("test-model");
        let result = apply_options(params, &[("max_tokens".into(), "4096".into())]).unwrap();
        assert_eq!(result.max_tokens, Some(4096));
    }

    #[test]
    fn apply_options_top_p() {
        let params = GenerateParams::new("test-model");
        let result = apply_options(params, &[("top_p".into(), "0.9".into())]).unwrap();
        assert_eq!(result.top_p, Some(0.9));
    }

    #[test]
    fn apply_options_unknown_key_goes_to_provider_opts() {
        let params = GenerateParams::new("test-model");
        let result = apply_options(params, &[("custom_key".into(), "custom_val".into())]).unwrap();
        let opts = result.provider_options.unwrap();
        assert_eq!(opts["custom_key"], "custom_val");
    }

    #[test]
    fn apply_options_invalid_temperature_errors() {
        let params = GenerateParams::new("test-model");
        assert!(apply_options(params, &[("temperature".into(), "not_a_number".into())]).is_err());
    }

    #[test]
    fn apply_options_invalid_max_tokens_errors() {
        let params = GenerateParams::new("test-model");
        assert!(apply_options(params, &[("max_tokens".into(), "abc".into())]).is_err());
    }

    #[test]
    fn apply_options_empty() {
        let params = GenerateParams::new("test-model");
        let result = apply_options(params, &[]).unwrap();
        assert_eq!(result.temperature, None);
        assert_eq!(result.max_tokens, None);
        assert_eq!(result.provider_options, None);
    }

    // --- test_model_via_server ---

    #[tokio::test]
    async fn test_model_via_server_parses_ok() {
        let server = httpmock::MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method("POST").path("/models/test-model/test");
                then.status(200)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({
                            "model_id": "test-model",
                            "status": "ok"
                        })
                        .to_string(),
                    );
            })
            .await;

        let client = reqwest::Client::new();
        let resp = test_model_via_server(&client, &server.url(""), "test-model")
            .await
            .unwrap();

        assert_eq!(resp.status, "ok");
        assert!(resp.error_message.is_none());
    }

    #[tokio::test]
    async fn test_model_via_server_parses_error() {
        let server = httpmock::MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method("POST").path("/models/test-model/test");
                then.status(200)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({
                            "model_id": "test-model",
                            "status": "error",
                            "error_message": "timeout"
                        })
                        .to_string(),
                    );
            })
            .await;

        let client = reqwest::Client::new();
        let resp = test_model_via_server(&client, &server.url(""), "test-model")
            .await
            .unwrap();

        assert_eq!(resp.status, "error");
        assert_eq!(resp.error_message.as_deref(), Some("timeout"));
    }

    #[tokio::test]
    async fn test_model_via_server_404() {
        let server = httpmock::MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method("POST").path("/models/bad-model/test");
                then.status(404)
                .header("Content-Type", "application/json")
                .body(serde_json::json!({
                    "errors": [{"status": "404", "title": "Not Found", "detail": "Model not found"}]
                }).to_string());
            })
            .await;

        let client = reqwest::Client::new();
        let result = test_model_via_server(&client, &server.url(""), "bad-model").await;
        assert!(result.is_err());
    }

    // --- fetch_models_from_server ---

    #[tokio::test]
    async fn fetch_models_from_server_parses_response() {
        let server = httpmock::MockServer::start_async().await;
        let mock = server.mock_async(|when, then| {
            when.method("GET").path("/models").query_param("page[limit]", "100");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(serde_json::json!({
                    "data": [{
                        "id": "test-model",
                        "provider": "anthropic",
                        "family": "test",
                        "display_name": "Test Model",
                        "limits": { "context_window": 128000, "max_output": 4096 },
                        "training": null,
                        "features": { "tools": true, "vision": false, "reasoning": false },
                        "costs": { "input_cost_per_mtok": 1.0, "output_cost_per_mtok": 2.0, "cache_input_cost_per_mtok": null },
                        "estimated_output_tps": 100.0,
                        "aliases": ["tm"],
                        "default": false
                    }],
                    "meta": { "has_more": false }
                }).to_string());
        }).await;

        let client = reqwest::Client::new();
        let models = fetch_models_from_server(&client, &server.url(""), None)
            .await
            .unwrap();

        mock.assert_async().await;
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "test-model");
        assert_eq!(models[0].provider, fabro_model::Provider::Anthropic);
    }

    #[tokio::test]
    async fn fetch_models_from_server_filters_by_provider() {
        let server = httpmock::MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method("GET").path("/models");
                then.status(200)
                    .header("Content-Type", "application/json")
                    .body(
                    serde_json::json!({
                        "data": [
                            {
                                "id": "model-a",
                                "provider": "anthropic",
                                "family": "a",
                                "display_name": "Model A",
                                "limits": { "context_window": 8000 },
                                "features": { "tools": false, "vision": false, "reasoning": false },
                                "costs": {},
                                "aliases": [],
                                "default": false
                            },
                            {
                                "id": "model-b",
                                "provider": "openai",
                                "family": "b",
                                "display_name": "Model B",
                                "limits": { "context_window": 8000 },
                                "features": { "tools": false, "vision": false, "reasoning": false },
                                "costs": {},
                                "aliases": [],
                                "default": false
                            }
                        ],
                        "meta": { "has_more": false }
                    })
                    .to_string(),
                );
            })
            .await;

        let client = reqwest::Client::new();
        let models = fetch_models_from_server(&client, &server.url(""), Some("anthropic"))
            .await
            .unwrap();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "model-a");
    }

    #[tokio::test]
    async fn fetch_models_from_server_error_on_failure() {
        let server = httpmock::MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method("GET").path("/models");
                then.status(500).body("internal error");
            })
            .await;

        let client = reqwest::Client::new();
        let result = fetch_models_from_server(&client, &server.url(""), None).await;
        assert!(result.is_err());
    }

    // --- run_prompt_via_server ---

    #[tokio::test]
    async fn run_prompt_via_server_non_streaming() {
        let mock_server = httpmock::MockServer::start_async().await;
        let mock = mock_server
            .mock_async(|when, then| {
                when.method("POST").path("/completions");
                then.status(200)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({
                            "id": "msg_123",
                            "model": "test-model",
                            "content": "Hello world",
                            "stop_reason": "end_turn",
                            "usage": {"input_tokens": 10, "output_tokens": 5}
                        })
                        .to_string(),
                    );
            })
            .await;

        let server = ServerConnection {
            client: reqwest::Client::new(),
            base_url: mock_server.url(""),
        };

        let args = PromptArgs {
            prompt: Some("Hello".into()),
            model: Some("test-model".into()),
            system: None,
            no_stream: true,
            usage: false,
            schema: None,
            option: vec![],
        };

        let result = run_prompt_via_server(args, &server).await;
        assert!(result.is_ok());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn run_prompt_via_server_streaming() {
        let mock_server = httpmock::MockServer::start_async().await;
        let sse_body = "\
event: stream_event\n\
data: {\"type\":\"text_delta\",\"delta\":\"Hi\",\"text_id\":null}\n\
\n\
event: stream_event\n\
data: {\"type\":\"finish\",\"finish_reason\":\"stop\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7},\"response\":{\"id\":\"r1\",\"model\":\"test\",\"provider\":\"test\",\"message\":{\"role\":\"assistant\",\"content\":[{\"kind\":\"text\",\"data\":\"Hi\"}],\"name\":null,\"tool_call_id\":null},\"finish_reason\":\"stop\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7},\"raw\":null,\"warnings\":[],\"rate_limit\":null}}\n\
\n";

        let mock = mock_server
            .mock_async(|when, then| {
                when.method("POST").path("/completions");
                then.status(200)
                    .header("Content-Type", "text/event-stream")
                    .body(sse_body);
            })
            .await;

        let server = ServerConnection {
            client: reqwest::Client::new(),
            base_url: mock_server.url(""),
        };

        let args = PromptArgs {
            prompt: Some("Hello".into()),
            model: Some("test-model".into()),
            system: None,
            no_stream: false,
            usage: false,
            schema: None,
            option: vec![],
        };

        let result = run_prompt_via_server(args, &server).await;
        assert!(result.is_ok());
        mock.assert_async().await;
    }
}
