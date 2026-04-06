use anyhow::Result;

use crate::args::{GlobalArgs, SystemInfoArgs};
use crate::server_client;
use crate::shared::print_json_pretty;

pub(super) async fn info_command(args: &SystemInfoArgs, globals: &GlobalArgs) -> Result<()> {
    let client = server_client::connect_server_backed_api_client_with_storage_dir(
        &args.connection.target,
        args.connection.storage_dir.as_deref(),
    )
    .await?;
    let response = client
        .get_system_info()
        .send()
        .await
        .map_err(server_client::map_api_error)?
        .into_inner();

    if globals.json {
        print_json_pretty(&response)?;
        return Ok(());
    }

    #[allow(clippy::print_stdout)]
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
        println!(
            "Sandbox: {}",
            response.sandbox_provider.as_deref().unwrap_or("unknown")
        );
        println!("Uptime: {}s", response.uptime_secs.unwrap_or_default());
    }

    Ok(())
}
