use anyhow::Result;

use crate::args::SecretSetArgs;
use fabro_config::dotenv;

pub fn set_command(args: &SecretSetArgs) -> Result<()> {
    let path = dotenv::env_file_path()?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let merged = dotenv::merge_env(&existing, &[(&args.key, &args.value)]);
    dotenv::write_env_file(&path, &merged)?;
    eprintln!("Set {}", args.key);
    Ok(())
}
