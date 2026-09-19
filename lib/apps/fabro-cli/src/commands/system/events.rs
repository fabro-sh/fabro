use anyhow::Result;
use fabro_client::sse;
use fabro_types::RunStreamItem;
use futures::StreamExt;

use crate::args::SystemEventsArgs;
use crate::command_context::CommandContext;

pub(super) async fn events_command(
    args: &SystemEventsArgs,
    base_ctx: &CommandContext,
) -> Result<()> {
    let ctx = base_ctx.with_connection(&args.connection)?;
    let server = ctx.server().await?;
    let mut stream = server.attach_events(&args.run_ids).await?;
    let mut pending = Vec::new();

    let json = ctx.json_output();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(anyhow::Error::new)?;
        pending.extend_from_slice(&chunk);
        for payload in sse::drain_sse_payloads(&mut pending, false) {
            render_sse_payload(&payload, json)?;
        }
    }

    for payload in sse::drain_sse_payloads(&mut pending, true) {
        render_sse_payload(&payload, json)?;
    }

    Ok(())
}

fn render_sse_payload(data: &str, json_output: bool) -> Result<()> {
    if json_output {
        #[allow(
            clippy::print_stdout,
            reason = "Raw event JSON belongs on stdout for piping."
        )]
        {
            println!("{data}");
        }
        return Ok(());
    }

    // Each frame is one `RunStreamItem`: the run, when its record was
    // appended, and the event's name.
    let item: RunStreamItem = serde_json::from_str(data)?;
    let recorded_at = i64::try_from(item.recorded_at)
        .ok()
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map_or_else(|| "-".to_string(), |at| at.to_rfc3339());
    let run_id = item.run_id.to_string();
    let event = item.name().unwrap_or("-");

    #[allow(
        clippy::print_stdout,
        reason = "Rendered event lines belong on stdout for piping."
    )]
    {
        println!("{recorded_at} {} {event}", short_run_id(&run_id));
    }
    Ok(())
}

fn short_run_id(run_id: &str) -> &str {
    run_id.get(..12).unwrap_or(run_id)
}
