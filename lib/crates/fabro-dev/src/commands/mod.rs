mod bench_tests;
mod build;
mod docker_build;
mod docs;
mod docs_cli_reference;
mod docs_options_reference;
mod release;
mod spa;
mod spa_check;
mod spa_refresh;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result};
pub(crate) use bench_tests::{BenchTestsArgs, bench_tests};
pub(crate) use build::{BuildArgs, build};
pub(crate) use docker_build::{DockerBuildArgs, docker_build};
pub(crate) use docs::{DocsArgs, docs};
pub(crate) use release::{ReleaseArgs, release};
pub(crate) use spa::{SpaArgs, spa};

pub(crate) fn workspace_root() -> PathBuf {
    let mut root = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    root.pop();
    root.pop();
    root.pop();
    root
}

pub(crate) fn markdown_cell(value: &str) -> String {
    value
        .replace('|', "\\|")
        .replace('\n', "<br />")
        .trim()
        .to_string()
}

pub(crate) fn replace_generated_region(
    current: &str,
    generated: &str,
    doc_path: &str,
    fence_start: &str,
    fence_end: &str,
) -> Result<String> {
    let start = current
        .find(fence_start)
        .with_context(|| format!("{doc_path} is missing {fence_start}"))?;
    let content_start = start + fence_start.len();
    let relative_end = current[content_start..]
        .find(fence_end)
        .with_context(|| format!("{doc_path} is missing {fence_end}"))?;
    let end = content_start + relative_end;

    let before = &current[..content_start];
    let after = &current[end..];
    Ok(format!("{before}\n{generated}\n{after}"))
}

pub(crate) struct PlannedCommand {
    pub(crate) program:   String,
    pub(crate) args:      Vec<String>,
    pub(crate) unset_env: Vec<String>,
    pub(crate) env:       Vec<(String, String)>,
}

impl PlannedCommand {
    pub(crate) fn new(program: impl Into<String>) -> Self {
        Self {
            program:   program.into(),
            args:      Vec::new(),
            unset_env: Vec::new(),
            env:       Vec::new(),
        }
    }

    pub(crate) fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub(crate) fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub(crate) fn env_remove(mut self, key: impl Into<String>) -> Self {
        self.unset_env.push(key.into());
        self
    }

    pub(crate) fn to_shell_line(&self) -> String {
        let mut parts = Vec::new();

        if !self.unset_env.is_empty() {
            parts.push("unset".to_string());
            parts.extend(self.unset_env.iter().map(shell_arg));
            parts.push("&&".to_string());
        }

        parts.extend(
            self.env
                .iter()
                .map(|(key, value)| format!("{}={}", shell_arg(key), shell_arg(value))),
        );
        parts.push(shell_arg(&self.program));
        parts.extend(self.args.iter().map(shell_arg));
        parts.join(" ")
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "fabro-dev intentionally builds synchronous subprocess commands"
)]
pub(crate) fn command(planned: &PlannedCommand) -> Command {
    let mut command = Command::new(&planned.program);
    command.args(&planned.args);
    for key in &planned.unset_env {
        command.env_remove(key);
    }
    for (key, value) in &planned.env {
        command.env(key, value);
    }
    command
}

pub(crate) fn run_command(cwd: &Path, planned: &PlannedCommand) -> Result<()> {
    let status = command(planned)
        .current_dir(cwd)
        .status()
        .with_context(|| format!("running {}", planned.to_shell_line()))?;
    if !status.success() {
        anyhow::bail!("command failed with {status}: {}", planned.to_shell_line());
    }

    Ok(())
}

pub(crate) fn capture_command(cwd: &Path, planned: &PlannedCommand) -> Result<Output> {
    command(planned)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("running {}", planned.to_shell_line()))
}

pub(crate) fn shell_arg(arg: impl AsRef<str>) -> String {
    let arg = arg.as_ref();
    shlex::try_quote(arg).map_or_else(
        |_| format!("'{}'", arg.replace('\'', "'\\''")),
        std::borrow::Cow::into_owned,
    )
}
