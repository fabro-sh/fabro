use std::io::Write;
use std::path::Path;

use crate::args::{GlobalArgs, SettingsArgs};
use crate::user_config;
use fabro_config::{ConfigLayer, FabroSettings};

fn merged_config(workflow: Option<&Path>, globals: &GlobalArgs) -> anyhow::Result<FabroSettings> {
    let cwd = std::env::current_dir()?;
    let base = match workflow {
        Some(path) => ConfigLayer::for_workflow(path, &cwd)?,
        None => ConfigLayer::project(&cwd)?,
    };
    let cli = user_config::user_layer_with_globals(globals)?;

    base.combine(cli).resolve()
}

pub(crate) fn execute(args: &SettingsArgs, globals: &GlobalArgs) -> anyhow::Result<()> {
    let config = merged_config(args.workflow.as_deref(), globals)?;
    let mut yaml = serde_yaml::to_string(&config)?;
    if !yaml.ends_with('\n') {
        yaml.push('\n');
    }

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle.write_all(yaml.as_bytes())?;

    Ok(())
}
