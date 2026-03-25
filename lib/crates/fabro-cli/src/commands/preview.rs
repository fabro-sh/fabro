use anyhow::{Context, Result};
use clap::Args;
use tracing::info;

use super::shared::validate_daytona_provider;

#[derive(Args)]
pub struct PreviewArgs {
    /// Run ID or prefix
    pub run: String,
    /// Port number
    pub port: u16,
    /// Generate a signed URL (embeds auth token, no headers needed)
    #[arg(long)]
    pub signed: bool,
    /// Signed URL expiry in seconds (default 3600, requires --signed)
    #[arg(long, default_value = "3600", requires = "signed")]
    pub ttl: i32,
    /// Open URL in browser (implies --signed)
    #[arg(long)]
    pub open: bool,
}

impl PreviewArgs {
    fn use_signed(&self) -> bool {
        self.signed || self.open
    }
}

pub async fn run(args: PreviewArgs) -> Result<()> {
    let base = fabro_workflows::run_lookup::default_runs_base();
    let run_dir = fabro_workflows::run_lookup::resolve_run(&base, &args.run)?.path;
    let sandbox_json = run_dir.join("sandbox.json");
    let record = fabro_workflows::records::SandboxRecord::load(&sandbox_json).context(
        "Failed to load sandbox.json — was this run started with a recent version of arc?",
    )?;

    validate_daytona_provider(&record, "Preview URLs")?;

    let name = record
        .identifier
        .as_deref()
        .context("Daytona sandbox record missing identifier (sandbox name)")?;

    info!(run_id = %args.run, provider = %record.provider, port = args.port, "Generating preview URL");

    let daytona = fabro_sandbox::daytona::DaytonaSandbox::reconnect(name)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if args.use_signed() {
        let signed = daytona
            .get_signed_preview_url(args.port, Some(args.ttl))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        print!("{}", format_signed_output(&signed.url));

        if args.open {
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
        print!("{}", format_standard_output(&preview.url, &preview.token));
    }

    Ok(())
}

fn format_standard_output(url: &str, token: &str) -> String {
    let mut out = format!("URL:   {url}\nToken: {token}\n");
    out.push_str(&format!(
        "\ncurl -H \"x-daytona-preview-token: {token}\" \\\n     -H \"X-Daytona-Skip-Preview-Warning: true\" \\\n     {url}\n"
    ));
    out
}

fn format_signed_output(url: &str) -> String {
    format!("{url}\n")
}
