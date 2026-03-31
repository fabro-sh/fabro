use anyhow::{Result, bail};

use crate::args::{GlobalArgs, SecretGetArgs};
use crate::shared::print_json_pretty;
use fabro_config::dotenv;

pub(super) fn get_command(args: &SecretGetArgs, globals: &GlobalArgs) -> Result<()> {
    let path = dotenv::env_file_path()?;
    match dotenv::get_env_value(&path, &args.key)? {
        Some(value) => {
            if globals.json {
                print_json_pretty(&serde_json::json!({
                    "key": args.key,
                    "value": value,
                }))?;
            } else {
                println!("{value}");
            }
            Ok(())
        }
        None => bail!("secret not found: {}", args.key),
    }
}
