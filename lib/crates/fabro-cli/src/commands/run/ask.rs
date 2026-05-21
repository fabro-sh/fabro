use anyhow::{Result, bail};
use fabro_api::types::CreateRunSessionRequest;
use fabro_store::EventEnvelope;

use crate::args::AskArgs;
use crate::command_context::CommandContext;

pub(crate) async fn run(args: AskArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run).await?.id;
    let session = client
        .create_run_session(run_id, CreateRunSessionRequest {
            title: Some(session_title(&args.prompt)),
            model: args.model,
        })
        .await?;
    let mut stream = client
        .submit_session_turn_stream(session.id, args.prompt)
        .await?;

    let mut terminal_error = None;
    let mut saw_terminal = false;
    while let Some(event) = stream.next_event().await? {
        render_event(&event, ctx.json_output())?;
        match event.event.event_name() {
            "run.session.turn.succeeded" | "run.session.turn.interrupted" => {
                saw_terminal = true;
            }
            "run.session.turn.failed" => {
                saw_terminal = true;
                terminal_error = Some(
                    event
                        .event
                        .properties()?
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("session turn failed")
                        .to_string(),
                );
            }
            _ => {}
        }
    }

    if let Some(error) = terminal_error {
        bail!(error);
    }
    if !saw_terminal {
        bail!("session turn ended before a terminal event was received");
    }
    Ok(())
}

fn session_title(prompt: &str) -> String {
    const MAX_CHARS: usize = 80;
    let trimmed = prompt.trim();
    if trimmed.chars().count() <= MAX_CHARS {
        return trimmed.to_string();
    }
    let mut title = trimmed.chars().take(MAX_CHARS - 3).collect::<String>();
    title.push_str("...");
    title
}

#[allow(
    clippy::print_stdout,
    reason = "The ask command streams assistant output and JSON events to stdout."
)]
fn render_event(event: &EventEnvelope, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string(event)?);
        return Ok(());
    }

    match event.event.event_name() {
        "run.session.assistant_delta" => {
            let properties = event.event.properties()?;
            if let Some(delta) = properties.get("delta").and_then(serde_json::Value::as_str) {
                print!("{delta}");
            }
        }
        "run.session.assistant_message" => {
            let properties = event.event.properties()?;
            if let Some(text) = properties
                .get("text")
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.is_empty())
            {
                println!("{text}");
            }
        }
        _ => {}
    }
    Ok(())
}
