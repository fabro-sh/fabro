use anyhow::{Result, bail};

use crate::args::{GlobalArgs, SecretListArgs};
use crate::shared::print_json_pretty;
use fabro_config::dotenv;

pub(super) fn list_command(args: &SecretListArgs, globals: &GlobalArgs) -> Result<()> {
    let path = dotenv::env_file_path()?;
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if globals.json {
                print_json_pretty(&Vec::<serde_json::Value>::new())?;
            }
            return Ok(());
        }
        Err(e) => bail!("failed to read {}: {e}", path.display()),
    };
    let pairs = dotenv::parse_env(&contents);
    if globals.json {
        let values = pairs
            .into_iter()
            .map(|(key, value)| {
                if args.show_values {
                    serde_json::json!({ "key": key, "value": value })
                } else {
                    serde_json::json!({ "key": key })
                }
            })
            .collect::<Vec<_>>();
        print_json_pretty(&values)?;
        return Ok(());
    }
    for (key, value) in pairs {
        if args.show_values {
            println!("{key}={value}");
        } else {
            println!("{key}");
        }
    }
    Ok(())
}
