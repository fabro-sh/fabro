use anyhow::Result;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::{CliLayer, OutputFormat};
use fabro_util::printer::Printer;

use crate::args::SystemInfoArgs;
use crate::command_context::CommandContext;
use crate::server_client;
use crate::shared::print_json_pretty;

pub(super) async fn info_command(
    args: &SystemInfoArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    printer: Printer,
) -> Result<()> {
    let ctx = CommandContext::for_connection(&args.connection, printer, cli.clone(), cli_layer)?;
    let server = ctx.server().await?;
    let response = server
        .api()
        .get_system_info()
        .send()
        .await
        .map_err(server_client::map_api_error)?
        .into_inner();

    if cli.output.format == OutputFormat::Json {
        print_json_pretty(&response)?;
        return Ok(());
    }

    #[allow(
        clippy::print_stdout,
        reason = "The system info report belongs on stdout for piping."
    )]
    {
        println!(
            "Version: {}",
            response
                .version
                .as_deref()
                .unwrap_or(env!("CARGO_PKG_VERSION"))
        );
        println!(
            "Build: {} {}",
            response.git_sha.as_deref().unwrap_or("unknown"),
            response.build_date.as_deref().unwrap_or("unknown")
        );
        if let Some(profile) = response
            .profile
            .as_deref()
            .filter(|p| !p.is_empty() && *p != "release")
        {
            println!("Profile: {profile}");
        }
        println!(
            "Platform: {}/{}",
            response.os.as_deref().unwrap_or("unknown"),
            response.arch.as_deref().unwrap_or("unknown")
        );
        println!(
            "Storage: {} ({})",
            response.storage_dir.as_deref().unwrap_or("unknown"),
            response.storage_engine.as_deref().unwrap_or("unknown")
        );
        println!(
            "Runs: total={} active={}",
            response
                .runs
                .as_ref()
                .and_then(|runs| runs.total)
                .unwrap_or_default(),
            response
                .runs
                .as_ref()
                .and_then(|runs| runs.active)
                .unwrap_or_default()
        );
        println!("Uptime: {}s", response.uptime_secs.unwrap_or_default());
    }

    Ok(())
}
