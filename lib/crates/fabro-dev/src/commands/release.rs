use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveDate};
use clap::{Args, ValueEnum};

const RELEASE_EPOCH: &str = "2026-01-01";
const RELEASE_TEST_SEGMENT_WRITE_KEY: &str = "fake-for-local-smoke";

#[derive(Debug, Args)]
pub(crate) struct ReleaseArgs {
    /// Pre-release label to create.
    #[arg(value_enum)]
    prerelease_label: Option<PrereleaseLabel>,
    /// Print planned release steps without mutating git or running Cargo.
    #[arg(long)]
    dry_run:          bool,
    /// Skip the release-mode test smoke.
    #[arg(long)]
    skip_tests:       bool,
    /// Release date to use for version computation.
    #[arg(long, value_name = "YYYY-MM-DD", env = "FABRO_RELEASE_DATE")]
    release_date:     Option<NaiveDate>,
    /// Repository root to release.
    #[arg(long, hide = true)]
    root:             Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PrereleaseLabel {
    Nightly,
}

impl fmt::Display for PrereleaseLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nightly => formatter.write_str("nightly"),
        }
    }
}

struct ReleasePlan {
    prerelease_label: Option<PrereleaseLabel>,
    release_date:     NaiveDate,
    dry_run:          bool,
    skip_tests:       bool,
    root:             PathBuf,
}

#[expect(
    clippy::print_stdout,
    reason = "dev release command reports progress and dry-run commands directly"
)]
pub(crate) fn release(args: ReleaseArgs) -> Result<()> {
    let plan = ReleasePlan {
        prerelease_label: args.prerelease_label,
        release_date:     args
            .release_date
            .unwrap_or_else(|| Local::now().date_naive()),
        dry_run:          args.dry_run,
        skip_tests:       args.skip_tests,
        root:             args.root.unwrap_or_else(workspace_root),
    };

    let cargo_toml = plan.root.join("Cargo.toml");
    let current_version = read_current_version(&cargo_toml)?;
    println!("Current version: {current_version}");

    let base_version = plan.next_base_version()?;
    let new_version = plan.compute_release_version(&base_version)?;
    let tag = format!("v{new_version}");
    println!("Releasing {new_version} (tag {tag})");

    if plan.dry_run {
        plan.print_dry_run(&current_version, &new_version, &tag);
        return Ok(());
    }

    plan.ensure_clean_worktree()?;
    plan.verify_spa_assets()?;
    plan.verify_release_tests()?;
    update_version(&cargo_toml, &current_version, &new_version)?;
    println!("Updated {}", cargo_toml.display());

    plan.run_command(
        &PlannedCommand::new("cargo")
            .arg("update")
            .arg("--workspace"),
    )?;
    println!("Updated Cargo.lock");

    plan.run_command(
        &PlannedCommand::new("git")
            .arg("add")
            .arg("Cargo.toml")
            .arg("Cargo.lock"),
    )?;
    plan.run_command(
        &PlannedCommand::new("git")
            .arg("commit")
            .arg("-m")
            .arg(format!("Bump version to {new_version}")),
    )?;
    plan.run_command(
        &PlannedCommand::new("git")
            .arg("tag")
            .arg("-a")
            .arg(&tag)
            .arg("-m")
            .arg(&tag),
    )?;
    plan.run_command(
        &PlannedCommand::new("git")
            .arg("push")
            .arg("origin")
            .arg("main")
            .arg(&tag),
    )?;

    println!();
    println!("Released {tag}");
    println!("Watch the build: https://github.com/fabro-sh/fabro/actions");

    Ok(())
}

impl ReleasePlan {
    fn next_base_version(&self) -> Result<String> {
        let epoch = NaiveDate::parse_from_str(RELEASE_EPOCH, "%Y-%m-%d")
            .expect("release epoch should be a valid date");
        let days_since_epoch = self.release_date.signed_duration_since(epoch).num_days();
        if days_since_epoch < 0 {
            bail!(
                "release date {} predates {RELEASE_EPOCH}",
                self.release_date
            );
        }

        let minor = days_since_epoch + 100;
        let mut patch = 0;
        loop {
            let version = format!("0.{minor}.{patch}");
            if !self.tag_exists(&format!("v{version}"))? {
                return Ok(version);
            }
            patch += 1;
        }
    }

    fn compute_release_version(&self, base_version: &str) -> Result<String> {
        let Some(prerelease_label) = self.prerelease_label else {
            return Ok(base_version.to_string());
        };

        let mut prerelease_number = 0;
        loop {
            let version = format!("{base_version}-{prerelease_label}.{prerelease_number}");
            if !self.tag_exists(&format!("v{version}"))? {
                return Ok(version);
            }
            prerelease_number += 1;
        }
    }

    fn ensure_clean_worktree(&self) -> Result<()> {
        let output = self.capture_command(
            &PlannedCommand::new("git")
                .arg("status")
                .arg("--porcelain")
                .arg("--untracked-files=all"),
        )?;
        if !output.status.success() {
            bail!(
                "failed to inspect working tree: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if !output.stdout.is_empty() {
            bail!("working tree is dirty; commit or stash changes before releasing");
        }

        Ok(())
    }

    fn verify_spa_assets(&self) -> Result<()> {
        self.run_command(&Self::refresh_spa_command())?;
        let output = self.capture_command(&Self::spa_assets_diff_command())?;
        if !output.status.success() {
            bail!("fabro-spa assets are stale. Commit the refreshed assets before releasing.");
        }

        Ok(())
    }

    #[expect(
        clippy::print_stdout,
        reason = "dev release command reports release test progress directly"
    )]
    fn verify_release_tests(&self) -> Result<()> {
        if self.skip_tests {
            println!("--skip-tests set, skipping release-mode test smoke");
            return Ok(());
        }

        println!("Running release-mode test smoke (SEGMENT_WRITE_KEY baked in)...");
        self.run_command(&Self::release_tests_command())
    }

    #[expect(
        clippy::print_stdout,
        reason = "dev release command reports dry-run commands directly"
    )]
    fn print_dry_run(&self, current_version: &str, new_version: &str, tag: &str) {
        println!("DRY RUN: would verify SPA assets:");
        println!("{}", Self::refresh_spa_command().to_shell_line());
        println!("{}", Self::spa_assets_diff_command().to_shell_line());

        if self.skip_tests {
            println!("--skip-tests set, would skip release-mode test smoke");
        } else {
            println!("DRY RUN: would run release-mode test smoke:");
            println!("{}", Self::release_tests_command().to_shell_line());
        }

        println!("DRY RUN: would update Cargo.toml version {current_version} -> {new_version}");
        for command in [
            PlannedCommand::new("cargo")
                .arg("update")
                .arg("--workspace"),
            PlannedCommand::new("git")
                .arg("add")
                .arg("Cargo.toml")
                .arg("Cargo.lock"),
            PlannedCommand::new("git")
                .arg("commit")
                .arg("-m")
                .arg(format!("Bump version to {new_version}")),
            PlannedCommand::new("git")
                .arg("tag")
                .arg("-a")
                .arg(tag)
                .arg("-m")
                .arg(tag),
            PlannedCommand::new("git")
                .arg("push")
                .arg("origin")
                .arg("main")
                .arg(tag),
        ] {
            println!("{}", command.to_shell_line());
        }
    }

    fn refresh_spa_command() -> PlannedCommand {
        PlannedCommand::new("cargo").arg("dev").arg("refresh-spa")
    }

    fn spa_assets_diff_command() -> PlannedCommand {
        PlannedCommand::new("git")
            .arg("diff")
            .arg("--exit-code")
            .arg("--")
            .arg("lib/crates/fabro-spa/assets")
    }

    fn release_tests_command() -> PlannedCommand {
        PlannedCommand::new("cargo")
            .env("SEGMENT_WRITE_KEY", RELEASE_TEST_SEGMENT_WRITE_KEY)
            .arg("nextest")
            .arg("run")
            .arg("--workspace")
            .arg("--release")
            .arg("--profile")
            .arg("ci")
            .arg("--status-level")
            .arg("slow")
    }

    fn tag_exists(&self, tag: &str) -> Result<bool> {
        let output = self.capture_command(
            &PlannedCommand::new("git")
                .arg("rev-parse")
                .arg("--verify")
                .arg("--quiet")
                .arg(format!("refs/tags/{tag}")),
        )?;
        Ok(output.status.success())
    }

    fn run_command(&self, planned: &PlannedCommand) -> Result<()> {
        let status = command(planned)
            .current_dir(&self.root)
            .status()
            .with_context(|| format!("running {}", planned.to_shell_line()))?;
        if !status.success() {
            bail!("command failed with {status}: {}", planned.to_shell_line());
        }

        Ok(())
    }

    fn capture_command(&self, planned: &PlannedCommand) -> Result<Output> {
        command(planned)
            .current_dir(&self.root)
            .output()
            .with_context(|| format!("running {}", planned.to_shell_line()))
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "dev release reads the workspace manifest synchronously"
)]
fn read_current_version(cargo_toml: &Path) -> Result<String> {
    let contents = std::fs::read_to_string(cargo_toml)
        .with_context(|| format!("reading {}", cargo_toml.display()))?;
    for line in contents.lines().map(str::trim) {
        let Some(rest) = line.strip_prefix("version = \"") else {
            continue;
        };
        let Some(version) = rest.strip_suffix('"') else {
            continue;
        };
        return Ok(version.to_string());
    }

    bail!(
        "could not find workspace package version in {}",
        cargo_toml.display()
    )
}

#[expect(
    clippy::disallowed_methods,
    reason = "dev release updates the workspace manifest synchronously"
)]
fn update_version(cargo_toml: &Path, current_version: &str, new_version: &str) -> Result<()> {
    let contents = std::fs::read_to_string(cargo_toml)
        .with_context(|| format!("reading {}", cargo_toml.display()))?;
    let needle = format!("version = \"{current_version}\"");
    let replacement = format!("version = \"{new_version}\"");
    if !contents.contains(&needle) {
        bail!(
            "could not find current version {current_version} in {}",
            cargo_toml.display()
        );
    }

    std::fs::write(cargo_toml, contents.replacen(&needle, &replacement, 1))
        .with_context(|| format!("writing {}", cargo_toml.display()))
}

#[expect(
    clippy::disallowed_methods,
    reason = "dev release builds synchronous subprocess commands"
)]
fn command(planned: &PlannedCommand) -> Command {
    let mut command = Command::new(&planned.program);
    command.args(&planned.args);
    for (key, value) in &planned.env {
        command.env(key, value);
    }
    command
}

struct PlannedCommand {
    program: String,
    args:    Vec<String>,
    env:     Vec<(String, String)>,
}

impl PlannedCommand {
    fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args:    Vec::new(),
            env:     Vec::new(),
        }
    }

    fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    fn to_shell_line(&self) -> String {
        self.env
            .iter()
            .map(|(key, value)| format!("{}={}", shell_arg(key), shell_arg(value)))
            .chain(std::iter::once(shell_arg(&self.program)))
            .chain(self.args.iter().map(shell_arg))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn shell_arg(arg: impl AsRef<str>) -> String {
    let arg = arg.as_ref();
    if arg
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "_-./:=@".contains(ch))
    {
        return arg.to_string();
    }

    format!("'{}'", arg.replace('\'', "'\\''"))
}

fn workspace_root() -> PathBuf {
    let mut root = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    root.pop();
    root.pop();
    root.pop();
    root
}
