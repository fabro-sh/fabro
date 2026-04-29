use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveDate};
use clap::Args;

use super::{PlannedCommand, capture_command, run_command, spa_refresh, workspace_root};

const RELEASE_EPOCH: &str = "2026-01-01";
const RELEASE_TEST_SEGMENT_WRITE_KEY: &str = "fake-for-local-smoke";

#[derive(Debug, Args)]
pub(crate) struct ReleaseArgs {
    /// Cut a nightly prerelease instead of a stable release.
    #[arg(long)]
    nightly:      bool,
    /// Print planned release steps without mutating git or running Cargo.
    #[arg(long)]
    dry_run:      bool,
    /// Skip the release-mode test smoke.
    #[arg(long)]
    skip_tests:   bool,
    /// Release date to use for version computation.
    #[arg(long, value_name = "YYYY-MM-DD", env = "FABRO_RELEASE_DATE")]
    release_date: Option<NaiveDate>,
    /// Repository root to release.
    #[arg(long, hide = true)]
    root:         Option<PathBuf>,
}

struct ReleasePlan {
    nightly:      bool,
    release_date: NaiveDate,
    dry_run:      bool,
    skip_tests:   bool,
    root:         PathBuf,
}

#[expect(
    clippy::print_stdout,
    reason = "dev release command reports progress and dry-run commands directly"
)]
pub(crate) fn release(args: ReleaseArgs) -> Result<()> {
    let plan = ReleasePlan {
        nightly:      args.nightly,
        release_date: args
            .release_date
            .unwrap_or_else(|| Local::now().date_naive()),
        dry_run:      args.dry_run,
        skip_tests:   args.skip_tests,
        root:         args.root.unwrap_or_else(workspace_root),
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
    spa_refresh::spa_refresh_root(&plan.root)?;
    plan.verify_release_tests()?;
    update_version(&cargo_toml, &current_version, &new_version)?;
    println!("Updated {}", cargo_toml.display());

    run_command(
        &plan.root,
        &PlannedCommand::new("cargo")
            .arg("update")
            .arg("--workspace"),
    )?;
    println!("Updated Cargo.lock");

    run_command(
        &plan.root,
        &PlannedCommand::new("git")
            .arg("add")
            .arg("Cargo.toml")
            .arg("Cargo.lock"),
    )?;
    run_command(
        &plan.root,
        &PlannedCommand::new("git")
            .arg("commit")
            .arg("-m")
            .arg(format!("Bump version to {new_version}")),
    )?;
    run_command(
        &plan.root,
        &PlannedCommand::new("git")
            .arg("tag")
            .arg("-a")
            .arg(&tag)
            .arg("-m")
            .arg(&tag),
    )?;
    run_command(
        &plan.root,
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
        if !self.nightly {
            return Ok(base_version.to_string());
        }

        let mut prerelease_number = 0;
        loop {
            let version = format!("{base_version}-nightly.{prerelease_number}");
            if !self.tag_exists(&format!("v{version}"))? {
                return Ok(version);
            }
            prerelease_number += 1;
        }
    }

    fn ensure_clean_worktree(&self) -> Result<()> {
        let output = capture_command(
            &self.root,
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
        run_command(&self.root, &Self::release_tests_command())
    }

    #[expect(
        clippy::print_stdout,
        reason = "dev release command reports dry-run commands directly"
    )]
    fn print_dry_run(&self, current_version: &str, new_version: &str, tag: &str) {
        println!("DRY RUN: would refresh SPA assets:");
        println!("{}", Self::spa_refresh_command().to_shell_line());

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

    fn spa_refresh_command() -> PlannedCommand {
        PlannedCommand::new("cargo")
            .arg("dev")
            .arg("spa")
            .arg("refresh")
    }

    fn release_tests_command() -> PlannedCommand {
        PlannedCommand::new("cargo")
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
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
        let output = capture_command(
            &self.root,
            &PlannedCommand::new("git")
                .arg("rev-parse")
                .arg("--verify")
                .arg("--quiet")
                .arg(format!("refs/tags/{tag}")),
        )?;
        Ok(output.status.success())
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "dev release reads the workspace manifest synchronously"
)]
fn read_current_version(cargo_toml: &Path) -> Result<String> {
    let contents = std::fs::read_to_string(cargo_toml)
        .with_context(|| format!("reading {}", cargo_toml.display()))?;
    let manifest = contents
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parsing {}", cargo_toml.display()))?;
    workspace_package_version(&manifest, cargo_toml).map(ToOwned::to_owned)
}

#[expect(
    clippy::disallowed_methods,
    reason = "dev release updates the workspace manifest synchronously"
)]
fn update_version(cargo_toml: &Path, current_version: &str, new_version: &str) -> Result<()> {
    let contents = std::fs::read_to_string(cargo_toml)
        .with_context(|| format!("reading {}", cargo_toml.display()))?;
    let mut manifest = contents
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("parsing {}", cargo_toml.display()))?;
    let version = workspace_package_version(&manifest, cargo_toml)?;
    if version != current_version {
        bail!(
            "could not find current version {current_version} in {}",
            cargo_toml.display()
        );
    }

    manifest["workspace"]["package"]["version"] = toml_edit::value(new_version);
    std::fs::write(cargo_toml, manifest.to_string())
        .with_context(|| format!("writing {}", cargo_toml.display()))
}

fn workspace_package_version<'a>(
    manifest: &'a toml_edit::DocumentMut,
    cargo_toml: &Path,
) -> Result<&'a str> {
    manifest["workspace"]["package"]["version"]
        .as_str()
        .with_context(|| {
            format!(
                "could not find [workspace.package] version in {}",
                cargo_toml.display()
            )
        })
}
