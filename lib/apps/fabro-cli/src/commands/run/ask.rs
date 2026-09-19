use anyhow::{Result, bail};
use fabro_api::types::CreateRunSessionRequest;
use fabro_types::{SessionEvent, SessionEventBody};

use crate::args::AskArgs;
use crate::command_context::CommandContext;

pub(crate) async fn run(args: AskArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run).await?.id;
    let session = client
        .create_run_session(run_id, CreateRunSessionRequest {
            title:    Some(session_title(&args.prompt)),
            model:    args.model,
            provider: None,
        })
        .await?;
    let mut stream = client
        .submit_session_turn_stream(session.id, args.prompt)
        .await?;

    let mut terminal_error = None;
    let mut saw_terminal = false;
    while let Some(event) = stream.next_event().await? {
        render_event(&event, ctx.json_output())?;
        match &event.body {
            SessionEventBody::TurnSucceeded(_) | SessionEventBody::TurnInterrupted(_) => {
                saw_terminal = true;
            }
            SessionEventBody::TurnFailed(props) => {
                saw_terminal = true;
                terminal_error = Some(props.error.clone());
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
fn render_event(event: &SessionEvent, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string(event)?);
        return Ok(());
    }

    match &event.body {
        SessionEventBody::AssistantDelta(props) => {
            print!("{}", props.delta);
        }
        SessionEventBody::AssistantMessage(props) if !props.text.is_empty() => {
            println!("{}", props.text);
        }
        _ => {}
    }
    Ok(())
}
