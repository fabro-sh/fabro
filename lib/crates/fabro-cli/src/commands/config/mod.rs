use std::io::Write;
use std::path::Path;

use crate::args::{ConfigCommand, ConfigNamespace, ConfigShowArgs};
use anyhow::bail;
use fabro_config::FabroSettings;

pub fn dispatch(ns: ConfigNamespace) -> anyhow::Result<()> {
    match ns.command {
        ConfigCommand::Show(args) => show_command(&args),
    }
}

fn merged_config(workflow: Option<&Path>) -> anyhow::Result<FabroSettings> {
    let mut config = fabro_config::cli::load_cli_config(None)?;
    let cwd = std::env::current_dir()?;

    if let Some((_config_path, project_config)) =
        fabro_config::project::discover_project_config(&cwd)?
    {
        config = config.combine(project_config);
    }

    if let Some(workflow) = workflow {
        let (resolved_path, _dot_path, run_config) =
            crate::commands::run::execute::resolve_workflow_source(workflow)?;

        if let Some(run_config) = run_config {
            config = config.combine(run_config);
        } else if !resolved_path.is_file() {
            bail!("Workflow not found: {}", resolved_path.display());
        }
    }

    config.try_into()
}

pub fn show_command(args: &ConfigShowArgs) -> anyhow::Result<()> {
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
