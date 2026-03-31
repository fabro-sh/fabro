use anyhow::Result;

use crate::args::{GlobalArgs, SecretSetArgs};
use crate::shared::print_json_pretty;
use fabro_config::dotenv;

pub(super) fn set_command(args: &SecretSetArgs, globals: &GlobalArgs) -> Result<()> {
    let path = dotenv::env_file_path()?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let merged = dotenv::merge_env(&existing, &[(&args.key, &args.value)]);
    dotenv::write_env_file(&path, &merged)?;
    if globals.json {
        print_json_pretty(&serde_json::json!({ "key": args.key }))?;
    } else {
        eprintln!("Set {}", args.key);
    }
    Ok(())
}
