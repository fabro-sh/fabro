use std::io::Write;
use std::path::Path;

use crate::args::{ConfigCommand, ConfigNamespace, ConfigShowArgs};
use fabro_config::project::ResolveSettingsInput;
use fabro_config::{FabroConfig, FabroSettings};

pub fn dispatch(ns: ConfigNamespace) -> anyhow::Result<()> {
    match ns.command {
        ConfigCommand::Show(args) => show_command(&args),
    }
}

fn merged_config(workflow: Option<&Path>) -> anyhow::Result<FabroSettings> {
    if let Some(workflow) = workflow {
        let cli_config = fabro_config::cli::load_cli_config(None)?;
        let cwd = std::env::current_dir()?;
        return fabro_config::project::resolve_settings(ResolveSettingsInput {
            workflow_path: workflow.to_path_buf(),
            cwd,
            defaults: cli_config,
            overrides: FabroConfig::default(),
            apply_project_config: true,
        });
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
