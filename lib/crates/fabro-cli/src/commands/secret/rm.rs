use anyhow::{Result, bail};

use crate::args::{GlobalArgs, SecretRmArgs};
use crate::shared::print_json_pretty;
use fabro_config::dotenv;

pub(super) fn rm_command(args: &SecretRmArgs, globals: &GlobalArgs) -> Result<()> {
    let path = dotenv::env_file_path()?;
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("secret not found: {}", args.key)
        }
        Err(e) => bail!("failed to read {}: {e}", path.display()),
    };
    let updated = dotenv::remove_env_key(&contents, &args.key);
    match updated {
        Some(new_contents) => {
            dotenv::write_env_file(&path, &new_contents)?;
            if globals.json {
                print_json_pretty(&serde_json::json!({ "key": args.key }))?;
            } else {
                eprintln!("Removed {}", args.key);
            }
            Ok(())
        }
        None => bail!("secret not found: {}", args.key),
    }
}
