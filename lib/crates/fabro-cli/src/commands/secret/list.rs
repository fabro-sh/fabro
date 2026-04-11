use anyhow::Result;
use fabro_api::Client;
use fabro_util::printer::Printer;

use crate::args::{GlobalArgs, SecretListArgs};
use crate::server_client;
use crate::shared::print_json_pretty;

pub(super) async fn list_command(
    client: &Client,
    args: &SecretListArgs,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    let response = client
        .list_secrets()
        .send()
        .await
        .map_err(server_client::map_api_error)?;
    let secrets = response.into_inner().data;
    if globals.json {
        print_json_pretty(&secrets)?;
        return Ok(());
    }
    let _ = args;
    for secret in secrets {
        fabro_util::printout!(printer, "{}\t{}", secret.name, secret.updated_at);
    }
    Ok(())
}
