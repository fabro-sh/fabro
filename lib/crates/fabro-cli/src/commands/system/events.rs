use anyhow::Result;
use futures::StreamExt;

use crate::args::{GlobalArgs, SystemEventsArgs};
use crate::server_client;

pub(super) async fn events_command(args: &SystemEventsArgs, globals: &GlobalArgs) -> Result<()> {
    let client = server_client::connect_server_backed_api_client_with_storage_dir(
        &args.connection.target,
        args.connection.storage_dir.as_deref(),
    )
    .await?;

    let mut request = client.attach_events();
    if !args.run_ids.is_empty() {
        request = request.run_id(args.run_ids.join(","));
    }

    let response = request.send().await.map_err(server_client::map_api_error)?;
    let mut stream = response.into_inner();
    let mut pending = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| anyhow::anyhow!("{err}"))?;
        pending.extend_from_slice(&chunk);
        drain_sse_lines(&mut pending, globals.json)?;
    }

    if !pending.is_empty() {
        drain_sse_lines(&mut pending, globals.json)?;
    }

    Ok(())
}

fn drain_sse_lines(buffer: &mut Vec<u8>, json_output: bool) -> Result<()> {
    while let Some(pos) = buffer.iter().position(|byte| *byte == b'\n') {
        let line = buffer.drain(..=pos).collect::<Vec<_>>();
        let line = String::from_utf8_lossy(&line);
        let line = line.trim_end_matches(['\r', '\n']);
        if let Some(data) = line.strip_prefix("data:") {
            render_sse_payload(data.trim(), json_output)?;
        }
    }
    Ok(())
}

fn render_sse_payload(data: &str, json_output: bool) -> Result<()> {
    if json_output {
        #[allow(clippy::print_stdout)]
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

    #[allow(clippy::print_stdout)]
    {
        println!("{ts} {} {event}", short_run_id(run_id));
    }
    Ok(())
}

fn short_run_id(run_id: &str) -> &str {
    run_id.get(..12).unwrap_or(run_id)
}
