use anyhow::{Result, bail};
use fabro_api::types::CreateSessionRequest;
use fabro_types::settings::InterpString;
use fabro_types::settings::run::AgentPermissions;
use fabro_types::{PermissionLevel, SessionEventEnvelope};

use crate::args::SessionArgs;
use crate::command_context::CommandContext;

pub(crate) async fn execute(args: SessionArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_connection(&args.connection)?;
    let client = ctx.server().await?;
    let request = CreateSessionRequest {
        title:       Some(session_title(&args.prompt)),
        working_dir: Some(ctx.cwd().to_string_lossy().into_owned()),
        provider:    session_provider(&args, &ctx),
        model:       session_model(&args, &ctx),
        permissions: session_permissions(&args, &ctx),
    };
    let session = client.create_session(request).await?;
    let mut stream = client
        .submit_session_turn_stream(session.id, args.prompt)
        .await?;

    let mut terminal_error = None;
    let mut saw_terminal = false;
    while let Some(event) = stream.next_event().await? {
        render_event(&event, ctx.json_output())?;
        match event.event.as_str() {
            "turn.succeeded" | "turn.interrupted" => saw_terminal = true,
            "turn.failed" => {
                saw_terminal = true;
                terminal_error = Some(
                    event
                        .properties
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

fn session_provider(args: &SessionArgs, ctx: &CommandContext) -> Option<String> {
    args.provider.clone().or_else(|| {
        ctx.user_settings()
            .cli
            .exec
            .model
            .provider
            .as_ref()
            .map(InterpString::as_source)
    })
}

fn session_model(args: &SessionArgs, ctx: &CommandContext) -> Option<String> {
    args.model.clone().or_else(|| {
        ctx.user_settings()
            .cli
            .exec
            .model
            .name
            .as_ref()
            .map(InterpString::as_source)
    })
}

fn session_permissions(args: &SessionArgs, ctx: &CommandContext) -> PermissionLevel {
    args.permissions
        .or_else(|| {
            ctx.user_settings()
                .cli
                .exec
                .agent
                .permissions
                .map(|permissions| match permissions {
                    AgentPermissions::ReadOnly => PermissionLevel::ReadOnly,
                    AgentPermissions::ReadWrite => PermissionLevel::ReadWrite,
                    AgentPermissions::Full => PermissionLevel::Full,
                })
        })
        .unwrap_or(PermissionLevel::ReadWrite)
}

#[allow(
    clippy::print_stdout,
    reason = "The session command streams assistant output and JSON events to stdout."
)]
fn render_event(event: &SessionEventEnvelope, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string(event)?);
        return Ok(());
    }

    match event.event.as_str() {
        "turn.text_delta" => {
            if let Some(delta) = event
                .properties
                .get("delta")
                .and_then(serde_json::Value::as_str)
            {
                print!("{delta}");
            }
        }
        "turn.assistant_message" => {
            if let Some(text) = event
                .properties
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
