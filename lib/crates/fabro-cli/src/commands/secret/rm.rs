use anyhow::{Result, bail};

use crate::args::SecretRmArgs;
use fabro_config::dotenv;

pub(super) fn rm_command(args: &SecretRmArgs) -> Result<()> {
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
            eprintln!("Removed {}", args.key);
            Ok(())
        }
        None => bail!("secret not found: {}", args.key),
    }
}
