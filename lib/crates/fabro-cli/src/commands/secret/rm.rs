use anyhow::Result;
use fabro_api::{Client, types};
use fabro_util::printer::Printer;

use crate::args::{GlobalArgs, SecretRmArgs};
use crate::server_client;
use crate::shared::print_json_pretty;

pub(super) async fn rm_command(
    client: &Client,
    args: &SecretRmArgs,
    globals: &GlobalArgs,
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
    if globals.json {
        print_json_pretty(&serde_json::json!({ "key": args.key }))?;
    } else {
        fabro_util::printerr!(printer, "Removed {}", args.key);
    }
    Ok(())
}
