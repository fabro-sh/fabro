use anyhow::{Result, bail};

use crate::args::SecretListArgs;
use fabro_config::dotenv;

pub(super) fn list_command(args: &SecretListArgs) -> Result<()> {
    let path = dotenv::env_file_path()?;
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => bail!("failed to read {}: {e}", path.display()),
    };
    let pairs = dotenv::parse_env(&contents);
    for (key, value) in pairs {
        if args.show_values {
            println!("{key}={value}");
        } else {
            println!("{key}");
        }
    }
    Ok(())
}
