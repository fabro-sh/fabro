use anyhow::Result;
use fabro_api::{Client, types};
use fabro_util::printer::Printer;

use crate::args::{GlobalArgs, SecretSetArgs, SecretTypeArg};
use crate::server_client;
use crate::shared::print_json_pretty;

fn api_secret_type(secret_type: SecretTypeArg) -> types::SecretType {
    match secret_type {
        SecretTypeArg::Environment => types::SecretType::Environment,
        SecretTypeArg::File => types::SecretType::File,
    }
}

pub(super) async fn set_command(
    client: &Client,
    args: &SecretSetArgs,
    globals: &GlobalArgs,
    printer: Printer,
) -> Result<()> {
    let meta = client
        .create_secret()
        .body(types::CreateSecretRequest {
            name:        args.key.clone(),
            value:       args.value.clone(),
            type_:       api_secret_type(args.r#type),
            description: args.description.clone(),
        })
        .send()
        .await
        .map_err(server_client::map_api_error)?
        .into_inner();
    if globals.json {
        print_json_pretty(&meta)?;
    } else {
        fabro_util::printerr!(printer, "Set {}", meta.name);
    }
    Ok(())
}
