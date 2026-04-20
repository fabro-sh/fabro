use anyhow::Result;
use fabro_api::types;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::OutputFormat;
use fabro_util::printer::Printer;

use crate::args::SecretRmArgs;
use crate::server_client::ServerStoreClient;
use crate::shared::print_json_pretty;

pub(super) async fn rm_command(
    client: &ServerStoreClient,
    args: &SecretRmArgs,
    cli: &CliSettings,
    printer: Printer,
) -> Result<()> {
    client
        .send_api(|api| async move {
            api.delete_secret_by_name()
                .body(types::DeleteSecretRequest {
                    name: args.key.clone(),
                })
                .send()
                .await
        })
        .await?;
    if cli.output.format == OutputFormat::Json {
        print_json_pretty(&serde_json::json!({ "key": args.key }))?;
    } else {
        fabro_util::printerr!(printer, "Removed {}", args.key);
    }
    Ok(())
}
