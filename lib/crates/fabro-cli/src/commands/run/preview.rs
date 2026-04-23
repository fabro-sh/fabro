use anyhow::{Context, Result};
use fabro_types::settings::cli::OutputFormat;
use tracing::info;

use crate::args::PreviewArgs;
use crate::command_context::CommandContext;
use crate::shared::print_json_pretty;

pub(crate) async fn run(args: PreviewArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_target(&args.server)?;
    let printer = ctx.printer();
    let client = ctx.server().await?;
    let run_id = client.resolve_run(&args.run).await?.run_id;
    let expires_in_secs =
        u64::try_from(args.ttl).map_err(|_| anyhow::anyhow!("--ttl must be positive"))?;
    let response = client
        .generate_preview_url(
            &run_id,
            args.port,
            expires_in_secs,
            args.signed || args.open,
        )
        .await?;

    info!(run_id = %args.run, port = args.port, "Generating preview URL");

    if ctx.user_settings().cli.output.format == OutputFormat::Json {
        match response.token {
            Some(token) => {
                print_json_pretty(&serde_json::json!({ "url": response.url, "token": token }))?;
            }
            None => {
                print_json_pretty(&serde_json::json!({ "url": response.url }))?;
            }
        }
    } else if let Some(token) = response.token.as_deref() {
        {
            use std::fmt::Write as _;
            let _ = write!(
                printer.stdout(),
                "{}",
                format_standard_output(&response.url, token)
            );
        }
    } else {
        {
            use std::fmt::Write as _;
            let _ = write!(printer.stdout(), "{}", format_signed_output(&response.url));
        }
    }

    if args.open && !ctx.explicit_json_requested() {
        #[expect(
            clippy::disallowed_methods,
            reason = "Preview URL opening is a fire-and-forget OS integration, not a Tokio-managed child process."
        )]
        let _browser = std::process::Command::new("open")
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
