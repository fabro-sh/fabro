use std::collections::HashMap;

use anyhow::Result;
use fabro_config::run::LlmConfig;
use fabro_config::{ConfigLayer, sandbox as sandbox_config};
use fabro_sandbox::SandboxProvider;

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

impl TryFrom<&RunArgs> for ConfigLayer {
    type Error = anyhow::Error;

    fn try_from(args: &RunArgs) -> Result<Self, Self::Error> {
        let llm = if args.model.is_some() || args.provider.is_some() {
            Some(LlmConfig {
                model: args.model.clone(),
                provider: args.provider.clone(),
                fallbacks: None,
            })
        } else {
            None
        };
        let sandbox = if args.sandbox.is_some() || args.preserve_sandbox {
            Some(sandbox_config::SandboxConfig {
                provider: args
                    .sandbox
                    .map(Into::into)
                    .map(|provider: SandboxProvider| provider.to_string()),
                preserve: sparse_flag(args.preserve_sandbox),
                ..Default::default()
            })
        } else {
            None
        };

        Ok(Self {
            goal: args.goal.clone(),
            goal_file: args.goal_file.clone(),
            llm,
            sandbox,
            verbose: sparse_flag(args.verbose),
            dry_run: sparse_flag(args.dry_run),
            auto_approve: sparse_flag(args.auto_approve),
            no_retro: sparse_flag(args.no_retro),
            labels: parse_labels(&args.label),
            ..Default::default()
        })
    }
}

impl TryFrom<&PreflightArgs> for ConfigLayer {
    type Error = anyhow::Error;

    fn try_from(args: &PreflightArgs) -> Result<Self, Self::Error> {
        let llm = if args.model.is_some() || args.provider.is_some() {
            Some(LlmConfig {
                model: args.model.clone(),
                provider: args.provider.clone(),
                fallbacks: None,
            })
        } else {
            None
        };
        let sandbox = args.sandbox.map(|sandbox| sandbox_config::SandboxConfig {
            provider: Some(SandboxProvider::from(sandbox).to_string()),
            ..Default::default()
        });

        Ok(Self {
            goal: args.goal.clone(),
            goal_file: args.goal_file.clone(),
            llm,
            sandbox,
            verbose: sparse_flag(args.verbose),
            ..Default::default()
        })
    }
}
