use anyhow::{Context, Result};
use fabro_sandbox::daytona::DaytonaSandbox;
use fabro_workflow::run_lookup::{resolve_run_combined, runs_base};
use tracing::info;

use crate::args::{GlobalArgs, PreviewArgs};
use crate::shared::{print_json_pretty, validate_daytona_provider};
use crate::store;
use crate::user_config::load_user_settings_with_globals;

pub(crate) async fn run(args: PreviewArgs, globals: &GlobalArgs) -> Result<()> {
    let cli_settings = load_user_settings_with_globals(globals)?;
    let base = runs_base(&cli_settings.storage_dir());
    let store = store::build_store(&cli_settings.storage_dir())?;
    let run = resolve_run_combined(store.as_ref(), &base, &args.run).await?;
    let run_store = store::open_run_reader(&cli_settings.storage_dir(), &run.run_id()).await?;
    let record = run_store
        .state()
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
