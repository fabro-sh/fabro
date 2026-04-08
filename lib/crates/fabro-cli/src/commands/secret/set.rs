use anyhow::Result;
use fabro_api::{Client, types};

use crate::args::{GlobalArgs, SecretSetArgs};
use crate::server_client;
use crate::shared::print_json_pretty;

pub(super) async fn set_command(
    client: &Client,
    args: &SecretSetArgs,
    globals: &GlobalArgs,
) -> Result<()> {
    let meta = client
        .set_secret()
        .name(args.key.clone())
        .body(types::SetSecretRequest {
            value: args.value.clone(),
        })
        .send()
        .await
        .map_err(server_client::map_api_error)?
        .into_inner();
    if globals.json {
        print_json_pretty(&meta)?;
    } else {
        eprintln!("Set {}", meta.name);
    }
    Ok(())
}
