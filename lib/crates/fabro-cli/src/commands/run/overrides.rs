use std::collections::HashMap;

use anyhow::Result;
use fabro_config::ConfigLayer;
use fabro_sandbox::SandboxProvider;
use fabro_types::settings::v2::SettingsFile;
use fabro_types::settings::v2::interp::InterpString;
use fabro_types::settings::v2::run::{
    ApprovalMode, RunExecutionLayer, RunLayer, RunMode, RunModelLayer, RunSandboxLayer,
};

use crate::args::{PreflightArgs, RunArgs};

fn sparse_flag(value: bool) -> Option<bool> {
    value.then_some(true)
}

pub(crate) fn parse_labels(labels: &[String]) -> HashMap<String, String> {
    labels
        .iter()
        .filter_map(|label| label.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn model_from_args(model: Option<&str>, provider: Option<&str>) -> Option<RunModelLayer> {
    if model.is_none() && provider.is_none() {
        return None;
    }
    Some(RunModelLayer {
        provider: provider.map(InterpString::parse),
        name: model.map(InterpString::parse),
        fallbacks: Vec::new(),
    })
}

fn sandbox_layer(
    sandbox: Option<SandboxProvider>,
    preserve: Option<bool>,
) -> Option<RunSandboxLayer> {
    if sandbox.is_none() && preserve.is_none() {
        return None;
    }
    Some(RunSandboxLayer {
        provider: sandbox.map(|p| p.to_string()),
        preserve,
        ..RunSandboxLayer::default()
    })
}

fn execution_layer(
    dry_run: Option<bool>,
    auto_approve: Option<bool>,
    no_retro: Option<bool>,
) -> Option<RunExecutionLayer> {
    if dry_run.is_none() && auto_approve.is_none() && no_retro.is_none() {
        return None;
    }
    Some(RunExecutionLayer {
        mode: dry_run.map(|d| if d { RunMode::DryRun } else { RunMode::Normal }),
        approval: auto_approve.map(|a| {
            if a {
                ApprovalMode::Auto
            } else {
                ApprovalMode::Prompt
            }
        }),
        retros: no_retro.map(|nr| !nr),
    })
}

impl TryFrom<&RunArgs> for ConfigLayer {
    type Error = anyhow::Error;

    fn try_from(args: &RunArgs) -> Result<Self, Self::Error> {
        let model = model_from_args(args.model.as_deref(), args.provider.as_deref());
        let sandbox = sandbox_layer(
            args.sandbox.map(Into::into),
            sparse_flag(args.preserve_sandbox),
        );
        let execution = execution_layer(
            sparse_flag(args.dry_run),
            sparse_flag(args.auto_approve),
            sparse_flag(args.no_retro),
        );

        let mut metadata = parse_labels(&args.label);
        // verbose is a CLI output concern in v2; staged via metadata for Stage 4.
        if args.verbose {
            metadata.insert("fabro.verbose".into(), "true".into());
        }

        let run = RunLayer {
            goal: args.goal.as_deref().map(InterpString::parse),
            metadata,
            model,
            sandbox,
            execution,
            ..RunLayer::default()
        };

        // goal_file is not part of v2; fall through to Settings.goal_file via the bridge.
        // Stage 4 consumers that still consult goal_file read it from Settings.
        let _ = &args.goal_file;

        Ok(Self::from(SettingsFile {
            run: Some(run),
            ..SettingsFile::default()
        }))
    }
}

impl TryFrom<&PreflightArgs> for ConfigLayer {
    type Error = anyhow::Error;

    fn try_from(args: &PreflightArgs) -> Result<Self, Self::Error> {
        let model = model_from_args(args.model.as_deref(), args.provider.as_deref());
        let sandbox = args.sandbox.map(|s| RunSandboxLayer {
            provider: Some(SandboxProvider::from(s).to_string()),
            ..RunSandboxLayer::default()
        });

        let mut metadata = std::collections::HashMap::new();
        if args.verbose {
            metadata.insert("fabro.verbose".into(), "true".into());
        }

        let run = RunLayer {
            goal: args.goal.as_deref().map(InterpString::parse),
            metadata,
            model,
            sandbox,
            ..RunLayer::default()
        };

        let _ = &args.goal_file; // Stage 4 preflight still reads goal_file via Settings bridge.

        Ok(Self::from(SettingsFile {
            run: Some(run),
            ..SettingsFile::default()
        }))
    }
}
