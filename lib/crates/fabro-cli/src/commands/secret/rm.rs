use anyhow::Result;
use fabro_api::{Client, types};
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::OutputFormat;
use fabro_util::printer::Printer;

use crate::args::SecretRmArgs;
use crate::server_client;
use crate::shared::print_json_pretty;

pub(super) async fn rm_command(
    client: &Client,
    args: &SecretRmArgs,
    cli: &CliSettings,
    printer: Printer,
) -> Result<()> {
    client
        .delete_secret_by_name()
        .body(types::DeleteSecretRequest {
            name: args.key.clone(),
        })
        .send()
        .await
        .map_err(server_client::map_api_error)?;
    if cli.output.format == OutputFormat::Json {
        print_json_pretty(&serde_json::json!({ "key": args.key }))?;
    } else {
        fabro_util::printerr!(printer, "Removed {}", args.key);
    }
    Ok(())
}
