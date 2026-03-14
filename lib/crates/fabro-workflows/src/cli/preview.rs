use anyhow::{bail, Context, Result};
use clap::Args;
use tracing::info;

use crate::cli::runs::{default_runs_base, resolve_run};
use crate::sandbox_record::SandboxRecord;

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

fn validate_provider(record: &SandboxRecord) -> Result<()> {
    if record.provider != "daytona" {
        bail!(
            "Preview URLs are only supported for Daytona sandboxes (this run uses '{}')",
            record.provider
        );
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

pub async fn preview_command(args: PreviewArgs) -> Result<()> {
    let base = default_runs_base();
    let run_dir = resolve_run(&base, &args.run)?.path;
    let sandbox_json = run_dir.join("sandbox.json");
    let record = SandboxRecord::load(&sandbox_json).context(
        "Failed to load sandbox.json — was this run started with a recent version of arc?",
    )?;

    validate_provider(&record)?;

    let name = record
        .identifier
        .as_deref()
        .context("Daytona sandbox record missing identifier (sandbox name)")?;

    info!(run_id = %args.run, provider = %record.provider, port = args.port, "Generating preview URL");

    let client = daytona_sdk::Client::new()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create Daytona client: {e}"))?;

    let sdk_sandbox = client
        .get(name)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to reconnect to Daytona sandbox '{name}': {e}"))?;

    let daytona = crate::daytona_sandbox::DaytonaSandbox::from_existing(client, sdk_sandbox);

    if args.use_signed() {
        let signed = daytona
            .get_signed_preview_url(args.port, Some(args.ttl))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let output = format_signed_output(&signed.url);
        print!("{output}");

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
        let output = format_standard_output(&preview.url, &preview.token);
        print!("{output}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_provider_rejects_local() {
        let record = SandboxRecord {
            provider: "local".to_string(),
            working_directory: "/tmp".to_string(),
            identifier: None,
            host_working_directory: None,
            container_mount_point: None,
            data_host: None,
        };
        let err = validate_provider(&record).unwrap_err();
        assert!(
            err.to_string()
                .contains("Preview URLs are only supported for Daytona sandboxes"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_provider_rejects_docker() {
        let record = SandboxRecord {
            provider: "docker".to_string(),
            working_directory: "/workspace".to_string(),
            identifier: None,
            host_working_directory: None,
            container_mount_point: None,
            data_host: None,
        };
        let err = validate_provider(&record).unwrap_err();
        assert!(
            err.to_string().contains("this run uses 'docker'"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_provider_accepts_daytona() {
        let record = SandboxRecord {
            provider: "daytona".to_string(),
            working_directory: "/home/daytona/workspace".to_string(),
            identifier: Some("sandbox-abc".to_string()),
            host_working_directory: None,
            container_mount_point: None,
            data_host: None,
        };
        validate_provider(&record).unwrap();
    }

    #[test]
    fn format_standard_output_includes_url_token_curl() {
        let output = format_standard_output(
            "https://3000-sandbox-123456.proxy.daytona.work",
            "vg5c0ylmcimr8b",
        );
        assert!(output.contains("URL:   https://3000-sandbox-123456.proxy.daytona.work"));
        assert!(output.contains("Token: vg5c0ylmcimr8b"));
        assert!(output.contains("curl"));
        assert!(output.contains("x-daytona-preview-token: vg5c0ylmcimr8b"));
        assert!(output.contains("X-Daytona-Skip-Preview-Warning: true"));
    }

    #[test]
    fn format_signed_output_is_just_url() {
        let output = format_signed_output("https://3000-eyJhbGci.proxy.daytona.work");
        assert_eq!(output, "https://3000-eyJhbGci.proxy.daytona.work\n");
    }

    #[test]
    fn use_signed_false_by_default() {
        let args = PreviewArgs {
            run: "abc".to_string(),
            port: 3000,
            signed: false,
            ttl: 3600,
            open: false,
        };
        assert!(!args.use_signed());
    }

    #[test]
    fn use_signed_true_when_signed() {
        let args = PreviewArgs {
            run: "abc".to_string(),
            port: 3000,
            signed: true,
            ttl: 3600,
            open: false,
        };
        assert!(args.use_signed());
    }

    #[test]
    fn use_signed_true_when_open() {
        let args = PreviewArgs {
            run: "abc".to_string(),
            port: 3000,
            signed: false,
            ttl: 3600,
            open: true,
        };
        assert!(args.use_signed());
    }
}
