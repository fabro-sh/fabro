use std::io::Write;
use std::path::Path;

use crate::args::{ConfigCommand, ConfigNamespace, ConfigShowArgs};
use anyhow::bail;
use fabro_config::{FabroConfig, FabroSettings};

pub fn dispatch(ns: ConfigNamespace) -> anyhow::Result<()> {
    match ns.command {
        ConfigCommand::Show(args) => show_command(&args),
    }
}

fn merged_config(workflow: Option<&Path>) -> anyhow::Result<FabroSettings> {
    if let Some(workflow) = workflow {
        let (resolved_path, _dot_path, run_config) =
            crate::commands::run::execute::resolve_workflow_source(workflow)?;
        let missing_workflow = run_config.is_none() && !resolved_path.is_file();
        let project_config = fabro_config::project::discover_project_config(
            resolved_path.parent().unwrap_or_else(|| Path::new(".")),
        )?
        .map(|(_, config)| config)
        .unwrap_or_default();
        let cli_config = fabro_config::cli::load_cli_config(None)?;
        let config = run_config
            .unwrap_or_default()
            .combine(project_config)
            .combine(cli_config);

        if missing_workflow {
            bail!("Workflow not found: {}", resolved_path.display());
        }

        return config.try_into();
    }

    let cwd = std::env::current_dir()?;
    let project_config = fabro_config::project::discover_project_config(&cwd)?
        .map(|(_, config)| config)
        .unwrap_or_default();
    let cli_config = fabro_config::cli::load_cli_config(None)?;
    FabroConfig::combine(project_config, cli_config).try_into()
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
