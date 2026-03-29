use std::io::Write;
use std::path::Path;

use crate::args::{ConfigCommand, ConfigNamespace, ConfigShowArgs};
use fabro_config::{ConfigLayer, FabroSettings};

pub(crate) fn dispatch(ns: ConfigNamespace) -> anyhow::Result<()> {
    match ns.command {
        ConfigCommand::Show(args) => show_command(&args),
    }
}

fn merged_config(workflow: Option<&Path>) -> anyhow::Result<FabroSettings> {
    let cwd = std::env::current_dir()?;
    let base = match workflow {
        Some(path) => ConfigLayer::for_workflow(path, &cwd)?,
        None => ConfigLayer::project(&cwd)?,
    };

    base.combine(ConfigLayer::cli()?).resolve()
}

pub(crate) fn show_command(args: &ConfigShowArgs) -> anyhow::Result<()> {
    let config = merged_config(args.workflow.as_deref())?;
    let mut yaml = serde_yaml::to_string(&config)?;
    if !yaml.ends_with('\n') {
        yaml.push('\n');
    }

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle.write_all(yaml.as_bytes())?;

    Ok(())
}
