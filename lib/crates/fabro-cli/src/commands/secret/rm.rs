use anyhow::Result;
use fabro_api::Client;

use crate::args::{GlobalArgs, SecretRmArgs};
use crate::shared::print_json_pretty;

pub(super) async fn rm_command(
    client: &Client,
    args: &SecretRmArgs,
    globals: &GlobalArgs,
) -> Result<()> {
    client
        .delete_secret()
        .name(args.key.clone())
        .send()
        .await
        .map_err(super::map_api_error)?;
    if globals.json {
        print_json_pretty(&serde_json::json!({ "key": args.key }))?;
    } else {
        eprintln!("Removed {}", args.key);
    }
    Ok(())
}
