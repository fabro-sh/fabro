use anyhow::{Result, bail};

use crate::args::SecretGetArgs;
use fabro_config::dotenv;

pub fn get_command(args: &SecretGetArgs) -> Result<()> {
    let path = dotenv::env_file_path()?;
    match dotenv::get_env_value(&path, &args.key)? {
        Some(value) => {
            println!("{value}");
            Ok(())
        }
        None => bail!("secret not found: {}", args.key),
    }
}
