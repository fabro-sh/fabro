use anyhow::{Context, Result};
use tracing::info;

use crate::args::{GlobalArgs, PreviewArgs};
use crate::server_runs::ServerSummaryLookup;
use crate::shared::print_json_pretty;

pub(crate) async fn run(args: PreviewArgs, globals: &GlobalArgs) -> Result<()> {
    let lookup = ServerSummaryLookup::connect(&args.server).await?;
    let run = lookup.resolve(&args.run)?;
    let run_id = run.run_id();
    let expires_in_secs =
        u64::try_from(args.ttl).map_err(|_| anyhow::anyhow!("--ttl must be positive"))?;
    let response = lookup
        .client()
        .generate_preview_url(
            &run_id,
            args.port,
            expires_in_secs,
            args.signed || args.open,
        )
        .await?;

    info!(run_id = %args.run, port = args.port, "Generating preview URL");

    if globals.json {
        match response.token {
            Some(token) => {
                print_json_pretty(&serde_json::json!({ "url": response.url, "token": token }))?;
            }
            None => {
                print_json_pretty(&serde_json::json!({ "url": response.url }))?;
            }
        }
    } else if let Some(token) = response.token.as_deref() {
        print!("{}", format_standard_output(&response.url, token));
    } else {
        print!("{}", format_signed_output(&response.url));
    }

    if args.open && !globals.json {
        std::process::Command::new("open")
            .arg(&response.url)
            .spawn()
            .context("Failed to open browser")?;
    }

    Ok(())
}

fn format_standard_output(url: &str, token: &str) -> String {
    use std::fmt::Write;
    let mut out = format!("URL:   {url}\nToken: {token}\n");
    let _ = write!(
        out,
        "\ncurl -H \"x-daytona-preview-token: {token}\" \\\n     -H \"X-Daytona-Skip-Preview-Warning: true\" \\\n     {url}\n"
    );
    out
}

fn format_signed_output(url: &str) -> String {
    format!("{url}\n")
}
