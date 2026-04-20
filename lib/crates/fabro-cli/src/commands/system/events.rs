use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;
use futures::StreamExt;

use crate::args::SystemEventsArgs;
use crate::command_context::CommandContext;
use crate::sse;

pub(super) async fn events_command(
    args: &SystemEventsArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let ctx = CommandContext::for_connection(&args.connection, printer, cli.clone(), cli_layer)?;
    let server = ctx.server().await?;
    let mut stream = server.attach_events(&args.run_ids).await?;
    let mut pending = Vec::new();

    let json = cli.output.format == OutputFormat::Json;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| anyhow::anyhow!("{err}"))?;
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

    let value: serde_json::Value = serde_json::from_str(data)?;
    let payload = value
        .get("payload")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let ts = payload
        .get("ts")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("-");
    let run_id = payload
        .get("run_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("-");
    let event = payload
        .get("event")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("-");

    #[allow(
        clippy::print_stdout,
        reason = "Rendered event lines belong on stdout for piping."
    )]
    {
        println!("{ts} {} {event}", short_run_id(run_id));
    }
    Ok(())
}

fn short_run_id(run_id: &str) -> &str {
    run_id.get(..12).unwrap_or(run_id)
}
