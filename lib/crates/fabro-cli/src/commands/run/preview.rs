use anyhow::{Context, Result};
use fabro_sandbox::daytona::DaytonaSandbox;
use tracing::info;

use crate::args::{GlobalArgs, PreviewArgs};
use crate::server_runs::ServerRunLookup;
use crate::shared::{print_json_pretty, validate_daytona_provider};
use crate::user_config::load_settings_with_storage_dir;

pub(crate) async fn run(args: PreviewArgs, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_settings_with_storage_dir(args.storage_dir.as_deref())?;
    let lookup = ServerRunLookup::connect(&cli_settings.storage_dir()).await?;
    let run = lookup.resolve(&args.run)?;
    let record = lookup
        .client()
        .get_run_state(&run.run_id())
        .await?
        .sandbox
        .context("Failed to load sandbox record from store")?;

    validate_daytona_provider(&record, "Preview URLs")?;

    let name = record
        .identifier
        .as_deref()
        .context("Daytona sandbox record missing identifier (sandbox name)")?;

    info!(run_id = %args.run, provider = %record.provider, port = args.port, "Generating preview URL");

    let daytona = DaytonaSandbox::reconnect(name)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if args.signed || args.open {
        let signed = daytona
            .get_signed_preview_url(args.port, Some(args.ttl))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        if globals.json {
            print_json_pretty(&serde_json::json!({ "url": signed.url }))?;
        } else {
            print!("{}", format_signed_output(&signed.url));
        }

        if args.open && !globals.json {
            std::process::Command::new("open")
                .arg(&signed.url)
                .spawn()
                .context("Failed to open browser")?;
        }
    } else {
        let preview = daytona
            .get_preview_link(args.port)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        if globals.json {
            print_json_pretty(&serde_json::json!({
                "url": preview.url,
                "token": preview.token,
            }))?;
        } else {
            print!("{}", format_standard_output(&preview.url, &preview.token));
        }
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
