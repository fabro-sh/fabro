use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use fabro_sandbox::SandboxProvider;
use fabro_types::settings::SettingsFile;
use fabro_types::settings::cli::{CliLayer, CliOutputLayer, OutputVerbosity};
use fabro_types::settings::interp::InterpString;
use fabro_types::settings::run::{
    ApprovalMode, RunExecutionLayer, RunGoalLayer, RunLayer, RunMode, RunModelLayer,
    RunSandboxLayer,
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

fn cli_layer_for_verbose(verbose: bool) -> Option<CliLayer> {
    verbose.then(|| CliLayer {
        output: Some(CliOutputLayer {
            verbosity: Some(OutputVerbosity::Verbose),
            ..CliOutputLayer::default()
        }),
        ..CliLayer::default()
    })
}

/// Build the `run.goal` override from the `--goal` / `--goal-file` args.
///
/// The two are mutually exclusive at the clap level; this helper assumes
/// at most one is set and returns an error if that invariant is violated.
///
/// CLI-supplied file paths are anchored at `cwd` (where the user invoked
/// the command), matching standard Unix CLI-flag conventions.
fn goal_layer_from_args(
    goal: Option<&str>,
    goal_file: Option<&Path>,
    cwd: &Path,
) -> Result<Option<RunGoalLayer>> {
    match (goal, goal_file) {
        (Some(_), Some(_)) => Err(anyhow!(
            "--goal and --goal-file are mutually exclusive; use exactly one"
        )),
        (Some(text), None) => Ok(Some(RunGoalLayer::Inline(InterpString::parse(text)))),
        (None, Some(path)) => {
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd.join(path)
            };
            Ok(Some(RunGoalLayer::File {
                file: InterpString::parse(&absolute.to_string_lossy()),
            }))
        }
        (None, None) => Ok(None),
    }
}

fn current_dir_or_dot() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub(crate) fn run_args_layer(args: &RunArgs) -> Result<SettingsFile> {
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

    let cwd = current_dir_or_dot();
    let goal = goal_layer_from_args(args.goal.as_deref(), args.goal_file.as_deref(), &cwd)?;

    let run = RunLayer {
        goal,
        metadata: parse_labels(&args.label),
        model,
        sandbox,
        execution,
        ..RunLayer::default()
    };

    Ok(SettingsFile {
        run: Some(run),
        cli: cli_layer_for_verbose(args.verbose),
        ..SettingsFile::default()
    })
}

pub(crate) fn preflight_args_layer(args: &PreflightArgs) -> Result<SettingsFile> {
    let model = model_from_args(args.model.as_deref(), args.provider.as_deref());
    let sandbox = args.sandbox.map(|s| RunSandboxLayer {
        provider: Some(SandboxProvider::from(s).to_string()),
        ..RunSandboxLayer::default()
    });

    let cwd = current_dir_or_dot();
    let goal = goal_layer_from_args(args.goal.as_deref(), args.goal_file.as_deref(), &cwd)?;

    let run = RunLayer {
        goal,
        model,
        sandbox,
        ..RunLayer::default()
    };

    Ok(SettingsFile {
        run: Some(run),
        cli: cli_layer_for_verbose(args.verbose),
        ..SettingsFile::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_and_goal_file_together_is_rejected() {
        let err = goal_layer_from_args(
            Some("inline text"),
            Some(Path::new("goal.md")),
            Path::new("/tmp"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn goal_file_is_anchored_at_cwd_when_relative() {
        let layer =
            goal_layer_from_args(None, Some(Path::new("prompts/goal.md")), Path::new("/cwd"))
                .unwrap()
                .expect("should build a goal layer");
        let RunGoalLayer::File { file } = layer else {
            panic!("expected file variant");
        };
        assert_eq!(file.as_source(), "/cwd/prompts/goal.md");
    }

    #[test]
    fn absolute_goal_file_is_preserved() {
        let layer = goal_layer_from_args(None, Some(Path::new("/abs/goal.md")), Path::new("/cwd"))
            .unwrap()
            .expect("should build a goal layer");
        let RunGoalLayer::File { file } = layer else {
            panic!("expected file variant");
        };
        assert_eq!(file.as_source(), "/abs/goal.md");
    }

    #[test]
    fn inline_goal_builds_inline_variant() {
        let layer = goal_layer_from_args(Some("inline goal"), None, Path::new("/cwd"))
            .unwrap()
            .expect("should build a goal layer");
        assert!(matches!(layer, RunGoalLayer::Inline(_)));
    }

    #[test]
    fn empty_args_produce_no_goal_layer() {
        assert!(
            goal_layer_from_args(None, None, Path::new("/cwd"))
                .unwrap()
                .is_none()
        );
    }
}
