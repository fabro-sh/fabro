use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use fabro_config::{
    CliLayer, CliOutputLayer, RunGoalLayer, RunLayer, parse_input_overrides, parse_labels,
};
use fabro_manifest::{RunOverrideInput, build_run_overrides};
use fabro_types::settings::cli::OutputVerbosity;
use fabro_types::settings::interp::InterpString;

use crate::args::{PreflightArgs, RunArgs};

#[derive(Clone, Debug, Default)]
pub(crate) struct ManifestSettingsOverrides {
    pub(crate) run:             Option<RunLayer>,
    pub(crate) cli:             Option<CliLayer>,
    pub(crate) input_overrides: HashMap<String, toml::Value>,
}

fn sparse_flag(value: bool) -> Option<bool> {
    value.then_some(true)
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

pub(crate) fn run_args_overrides(args: &RunArgs) -> Result<ManifestSettingsOverrides> {
    let cwd = current_dir_or_dot();
    let goal = goal_layer_from_args(args.goal.as_deref(), args.goal_file.as_deref(), &cwd)?;
    let mut run = build_run_overrides(RunOverrideInput {
        goal:             None,
        model:            args.model.as_deref(),
        provider:         args.provider.as_deref(),
        environment:      args.environment.as_deref(),
        docker_image:     None,
        preserve_sandbox: sparse_flag(args.preserve_sandbox),
        dry_run:          sparse_flag(args.dry_run),
        auto_approve:     sparse_flag(args.auto_approve),
        labels:           parse_labels(&args.label),
    });
    run.goal = goal;

    Ok(ManifestSettingsOverrides {
        run:             Some(run),
        cli:             cli_layer_for_verbose(args.verbose),
        input_overrides: parse_input_overrides(&args.inputs.values)?,
    })
}

pub(crate) fn preflight_args_overrides(args: &PreflightArgs) -> Result<ManifestSettingsOverrides> {
    let cwd = current_dir_or_dot();
    let goal = goal_layer_from_args(args.goal.as_deref(), args.goal_file.as_deref(), &cwd)?;
    let mut run = build_run_overrides(RunOverrideInput {
        goal:             None,
        model:            args.model.as_deref(),
        provider:         args.provider.as_deref(),
        environment:      args.environment.as_deref(),
        docker_image:     None,
        preserve_sandbox: None,
        dry_run:          None,
        auto_approve:     None,
        labels:           HashMap::new(),
    });
    run.goal = goal;

    Ok(ManifestSettingsOverrides {
        run:             Some(run),
        cli:             cli_layer_for_verbose(args.verbose),
        input_overrides: parse_input_overrides(&args.inputs.values)?,
    })
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "tests assert the raw template source"
)]
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
