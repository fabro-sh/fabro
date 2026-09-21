use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveDate};
use clap::Args;

use super::{PlannedCommand, capture_command, plugins, run_command, spa_refresh, workspace_root};

const RELEASE_EPOCH: &str = "2026-01-01";
const RELEASE_TEST_SEGMENT_WRITE_KEY: &str = "fake-for-local-smoke";
const MAX_PUSH_ATTEMPTS: u32 = 4;
const RELEASE_BRANCH: &str = "main";
const RELEASE_REMOTE: &str = "origin";

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

struct ReleaseVersions {
    current: String,
    next:    String,
}

impl ReleaseVersions {
    fn tag(&self) -> String {
        format!("v{}", self.next)
    }
}

struct RemoteReleaseState {
    main: String,
    tag:  Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum PushFailureDisposition {
    Published,
    Retryable,
    Unchanged,
    Inconsistent,
}

impl RemoteReleaseState {
    fn classify_push_failure(
        &self,
        remote_main_before: &str,
        release_head: &str,
        local_contains_remote_before: bool,
    ) -> PushFailureDisposition {
        let main_is_release = self.main == release_head;
        let tag_is_release = self.tag.as_deref() == Some(release_head);

        if main_is_release && tag_is_release {
            PushFailureDisposition::Published
        } else if main_is_release || tag_is_release {
            PushFailureDisposition::Inconsistent
        } else if !local_contains_remote_before
            || self.main != remote_main_before
            || self.tag.is_some()
        {
            PushFailureDisposition::Retryable
        } else {
            PushFailureDisposition::Unchanged
        }
    }
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
    let versions = plan.compute_versions(&cargo_toml)?;
    let tag = versions.tag();
    println!("Current version: {}", versions.current);
    println!("Releasing {} (tag {tag})", versions.next);

    if plan.dry_run {
        plan.print_dry_run(&versions);
        return Ok(());
    }

    plan.ensure_main_branch()?;
    plan.ensure_clean_worktree()?;
    spa_refresh::spa_refresh_root(&plan.root)?;
    plan.verify_release_tests()?;
    let tag = plan.commit_tag_and_push(&cargo_toml, versions)?;

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

    fn compute_versions(&self, cargo_toml: &Path) -> Result<ReleaseVersions> {
        let current = read_current_version(cargo_toml)?;
        let base_version = self.next_base_version()?;
        let next = self.compute_release_version(&base_version)?;
        Ok(ReleaseVersions { current, next })
    }

    /// Commits the version bump, tags it, and pushes `main` plus the tag
    /// atomically. When the push is rejected because origin/main moved while
    /// the release ran, rebuilds the bump commit and tag on the fresh tip
    /// and retries.
    #[expect(
        clippy::print_stdout,
        reason = "dev release command reports push retry progress directly"
    )]
    fn commit_tag_and_push(
        &self,
        cargo_toml: &Path,
        mut versions: ReleaseVersions,
    ) -> Result<String> {
        let mut attempt = 1;
        loop {
            self.ensure_main_branch()?;
            self.ensure_clean_worktree()
                .context("working tree changed while the release was running")?;

            let start_head = super::resolve_git_revision(&self.root, "HEAD")?;
            let remote_tracking_ref = Self::remote_tracking_ref();
            let remote_main_before =
                super::resolve_git_revision(&self.root, &remote_tracking_ref).with_context(|| {
                    format!(
                        "release requires a local {} tracking ref; fetch {RELEASE_REMOTE} and retry",
                        Self::remote_branch()
                    )
                })?;
            let local_contains_remote_before =
                self.commit_is_ancestor(&remote_main_before, &start_head)?;
            let tag = versions.tag();
            self.create_bump_commit_and_tag(cargo_toml, &versions)?;
            let release_head = super::resolve_git_revision(&self.root, "HEAD")?;
            let Err(push_error) = self.push_main_and_tag(&tag) else {
                return Ok(tag);
            };

            let disposition = self
                .push_failure_disposition(
                    &tag,
                    &remote_main_before,
                    &release_head,
                    local_contains_remote_before,
                )
                .context("push failed and origin could not be verified")?;
            match disposition {
                PushFailureDisposition::Published => {
                    println!(
                        "Push reported an error, but {RELEASE_REMOTE} contains {RELEASE_BRANCH} \
                         and {tag} at {release_head}"
                    );
                    return Ok(tag);
                }
                PushFailureDisposition::Unchanged => return Err(push_error),
                PushFailureDisposition::Inconsistent => {
                    return Err(push_error.context(
                        "origin contains only part of the release; inspect the remote refs before \
                         retrying",
                    ));
                }
                PushFailureDisposition::Retryable => {
                    if attempt == MAX_PUSH_ATTEMPTS {
                        return Err(push_error);
                    }
                }
            }

            let remote_branch = Self::remote_branch();
            println!(
                "Origin changed during push attempt {attempt} of {MAX_PUSH_ATTEMPTS}; rebuilding \
                 the release on the latest {remote_branch}"
            );
            self.resync_with_origin_main(&tag, &start_head, &release_head)?;
            versions = self.compute_versions(cargo_toml)?;
            println!("Retrying as {} (tag {})", versions.next, versions.tag());
            attempt += 1;
        }
    }

    #[expect(
        clippy::print_stdout,
        reason = "dev release command reports version bump progress directly"
    )]
    fn create_bump_commit_and_tag(
        &self,
        cargo_toml: &Path,
        versions: &ReleaseVersions,
    ) -> Result<()> {
        update_version(cargo_toml, &versions.current, &versions.next)?;
        println!("Updated {}", cargo_toml.display());

        let [cargo_update, git_add, git_commit, git_tag] = Self::bump_commands(versions);
        run_command(&self.root, &cargo_update)?;
        println!("Updated Cargo.lock");

        for command in [git_add, git_commit, git_tag] {
            run_command(&self.root, &command)?;
        }
        Ok(())
    }

    fn bump_commands(versions: &ReleaseVersions) -> [PlannedCommand; 4] {
        let tag = versions.tag();
        [
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
                .arg(format!("Bump version to {}", versions.next)),
            PlannedCommand::new("git")
                .arg("tag")
                .arg("-a")
                .arg(&tag)
                .arg("-m")
                .arg(tag),
        ]
    }

    fn push_main_and_tag(&self, tag: &str) -> Result<()> {
        run_command(&self.root, &Self::push_command(tag))
    }

    fn push_failure_disposition(
        &self,
        tag: &str,
        remote_main_before: &str,
        release_head: &str,
        local_contains_remote_before: bool,
    ) -> Result<PushFailureDisposition> {
        Ok(self.remote_release_state(tag)?.classify_push_failure(
            remote_main_before,
            release_head,
            local_contains_remote_before,
        ))
    }

    fn commit_is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        let output = capture_command(
            &self.root,
            &PlannedCommand::new("git")
                .arg("merge-base")
                .arg("--is-ancestor")
                .arg(ancestor)
                .arg(descendant),
        )?;
        if output.status.success() {
            return Ok(true);
        }
        if output.status.code() == Some(1) {
            return Ok(false);
        }
        bail!(
            "failed to compare git commits {ancestor} and {descendant}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }

    fn remote_release_state(&self, tag: &str) -> Result<RemoteReleaseState> {
        let branch_ref = Self::branch_ref();
        let tag_ref = Self::tag_ref(tag);
        let peeled_tag_ref = format!("{tag_ref}^{{}}");
        let output = capture_command(
            &self.root,
            &PlannedCommand::new("git")
                .arg("ls-remote")
                .arg(RELEASE_REMOTE)
                .arg(&branch_ref)
                .arg(&tag_ref)
                .arg(&peeled_tag_ref),
        )?;
        if !output.status.success() {
            bail!(
                "failed to inspect release refs on {RELEASE_REMOTE}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        let stdout =
            String::from_utf8(output.stdout).context("git ls-remote returned non-UTF-8 output")?;
        let mut main = None;
        let mut tag_object = None;
        let mut peeled_tag = None;
        for line in stdout.lines() {
            let (object_id, reference) = line
                .split_once('\t')
                .with_context(|| format!("unexpected git ls-remote output: {line}"))?;
            if reference == branch_ref {
                main = Some(object_id.to_string());
            } else if reference == tag_ref {
                tag_object = Some(object_id.to_string());
            } else if reference == peeled_tag_ref {
                peeled_tag = Some(object_id.to_string());
            }
        }

        let remote_branch = Self::remote_branch();
        let main = main
            .with_context(|| format!("{remote_branch} was not present in git ls-remote output"))?;
        Ok(RemoteReleaseState {
            main,
            tag: peeled_tag.or(tag_object),
        })
    }

    /// Drops the bump commit and tag this run created, then fast-forwards
    /// onto the updated origin/main. `--keep` preserves worktree edits, and
    /// `--ff-only` refuses to discard commits that did not come from origin.
    fn resync_with_origin_main(
        &self,
        tag: &str,
        start_head: &str,
        release_head: &str,
    ) -> Result<()> {
        self.ensure_main_branch()?;
        let current_head = super::resolve_git_revision(&self.root, "HEAD")?;
        if current_head != release_head {
            bail!(
                "local {RELEASE_BRANCH} changed while the release was running; refusing to move \
                 it from {current_head} back to {start_head}"
            );
        }

        let tag_ref = Self::tag_ref(tag);
        let tag_object = super::resolve_git_revision(&self.root, &tag_ref)
            .with_context(|| format!("local release tag {tag} changed while the release ran"))?;
        let tag_commit = super::resolve_git_revision(&self.root, &format!("{tag_ref}^{{commit}}"))
            .with_context(|| format!("local release tag {tag} changed while the release ran"))?;
        if tag_commit != release_head {
            bail!(
                "local release tag {tag} changed while the release was running; refusing to \
                 delete it"
            );
        }

        run_command(
            &self.root,
            &PlannedCommand::new("git")
                .arg("reset")
                .arg("--keep")
                .arg(start_head),
        )
        .context(
            "working tree changed while the release was running; local edits were preserved",
        )?;
        run_command(
            &self.root,
            &PlannedCommand::new("git")
                .arg("update-ref")
                .arg("-d")
                .arg(&tag_ref)
                .arg(tag_object),
        )
        .with_context(|| format!("local release tag {tag} changed while the release ran"))?;
        self.ensure_clean_worktree()
            .context("working tree changed while the release was running")?;
        run_command(
            &self.root,
            &PlannedCommand::new("git")
                .arg("fetch")
                .arg("--tags")
                .arg(RELEASE_REMOTE)
                .arg(RELEASE_BRANCH),
        )?;
        let remote_branch = Self::remote_branch();
        run_command(
            &self.root,
            &PlannedCommand::new("git")
                .arg("merge")
                .arg("--ff-only")
                .arg(&remote_branch),
        )
        .with_context(|| {
            format!(
                "local {RELEASE_BRANCH} has diverged from {remote_branch}; reconcile manually and \
                 rerun the release"
            )
        })?;
        self.ensure_clean_worktree()
            .context("working tree changed while the release was running")
    }

    fn branch_ref() -> String {
        format!("refs/heads/{RELEASE_BRANCH}")
    }

    fn remote_branch() -> String {
        format!("{RELEASE_REMOTE}/{RELEASE_BRANCH}")
    }

    fn remote_tracking_ref() -> String {
        format!("refs/remotes/{RELEASE_REMOTE}/{RELEASE_BRANCH}")
    }

    fn tag_ref(tag: &str) -> String {
        format!("refs/tags/{tag}")
    }

    fn push_command(tag: &str) -> PlannedCommand {
        PlannedCommand::new("git")
            .arg("push")
            .arg("--atomic")
            .arg(RELEASE_REMOTE)
            .arg(RELEASE_BRANCH)
            .arg(tag)
    }

    fn ensure_main_branch(&self) -> Result<()> {
        let output = capture_command(
            &self.root,
            &PlannedCommand::new("git")
                .arg("symbolic-ref")
                .arg("--quiet")
                .arg("HEAD"),
        )?;
        if !output.status.success() {
            bail!("release must run from {RELEASE_BRANCH}; HEAD is not a local branch");
        }

        let head_ref = String::from_utf8(output.stdout)
            .context("git symbolic-ref returned non-UTF-8 output")?;
        let head_ref = head_ref.trim();
        if head_ref != Self::branch_ref() {
            let branch = head_ref.strip_prefix("refs/heads/").unwrap_or(head_ref);
            bail!("release must run from {RELEASE_BRANCH}; current branch is {branch}");
        }
        Ok(())
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
        let directory = plugins::prepare(&self.root, None, false)?;
        // CLI diagnostics inspect the installed bundle beside the executable,
        // even when workflow tests use explicit plugin paths.
        let target_root = directory
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .context("plugin staging directory has no Cargo target root")?;
        plugins::stage(&directory, &target_root.join("release"))?;
        run_command(
            &self.root,
            &plugins::configure(Self::release_tests_command(), &directory),
        )
    }

    #[expect(
        clippy::print_stdout,
        reason = "dev release command reports dry-run commands directly"
    )]
    fn print_dry_run(&self, versions: &ReleaseVersions) {
        println!("DRY RUN: would refresh SPA assets:");
        println!("{}", Self::spa_refresh_command().to_shell_line());

        if self.skip_tests {
            println!("--skip-tests set, would skip release-mode test smoke");
        } else {
            println!("cargo --locked dev plugins");
            println!(
                "DRY RUN: would run release-mode test smoke with embedded plugin pins and explicit plugin paths:"
            );
            println!("{}", Self::release_tests_command().to_shell_line());
        }

        println!(
            "DRY RUN: would update Cargo.toml version {} -> {}",
            versions.current, versions.next
        );
        let tag = versions.tag();
        for command in Self::bump_commands(versions)
            .into_iter()
            .chain(std::iter::once(Self::push_command(&tag)))
        {
            println!("{}", command.to_shell_line());
        }
    }

    fn spa_refresh_command() -> PlannedCommand {
        PlannedCommand::new("cargo")
            .arg("--locked")
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
            .arg("--locked")
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

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE_MANIFEST: &str = r#"[workspace]
members = ["app"]

[workspace.package]
version = "0.1.0"
"#;

    const MEMBER_MANIFEST: &str = r#"[package]
name = "app"
edition = "2021"
version.workspace = true
"#;

    fn git(root: &Path, args: &[&str]) -> String {
        let mut command = PlannedCommand::new("git");
        for arg in args {
            command = command.arg(*arg);
        }
        let output = capture_command(root, &command).expect("git should spawn");
        assert!(
            output.status.success(),
            "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn configure_identity(repo: &Path) {
        git(repo, &["config", "user.name", "Release Test"]);
        git(repo, &["config", "user.email", "release-test@example.com"]);
    }

    /// A fixture with a bare `origin`, a `work` clone releases run from, and
    /// an `other` clone that simulates concurrent pushes.
    struct RaceFixture {
        _dir:   tempfile::TempDir,
        origin: PathBuf,
        work:   PathBuf,
        other:  PathBuf,
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "release tests build git fixture repositories synchronously"
    )]
    fn race_fixture() -> RaceFixture {
        let dir = tempfile::tempdir().expect("creating fixture");
        let origin = dir.path().join("origin.git");
        let work = dir.path().join("work");
        let other = dir.path().join("other");

        std::fs::create_dir(&origin).expect("creating origin dir");
        git(&origin, &["init", "--bare", "-b", "main"]);

        std::fs::create_dir(&work).expect("creating work dir");
        git(&work, &["init", "-b", "main"]);
        configure_identity(&work);
        std::fs::write(work.join("Cargo.toml"), WORKSPACE_MANIFEST).expect("writing manifest");
        std::fs::create_dir_all(work.join("app/src")).expect("creating member dirs");
        std::fs::write(work.join("app/Cargo.toml"), MEMBER_MANIFEST)
            .expect("writing member manifest");
        std::fs::write(work.join("app/src/lib.rs"), "").expect("writing member lib");
        git(&work, &["add", "."]);
        git(&work, &["commit", "-m", "initial"]);
        git(&work, &[
            "remote",
            "add",
            "origin",
            origin.to_str().expect("origin path should be utf-8"),
        ]);
        git(&work, &["push", "-u", "origin", "main"]);

        git(dir.path(), &[
            "clone",
            origin.to_str().expect("origin path should be utf-8"),
            "other",
        ]);
        configure_identity(&other);

        RaceFixture {
            _dir: dir,
            origin,
            work,
            other,
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "release tests write fixture files synchronously"
    )]
    fn write_file(path: &Path, contents: &str) {
        std::fs::write(path, contents).expect("writing fixture file");
    }

    fn nightly_plan(root: &Path) -> ReleasePlan {
        ReleasePlan {
            nightly:      true,
            release_date: NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid release date"),
            dry_run:      false,
            skip_tests:   true,
            root:         root.to_path_buf(),
        }
    }

    fn push_concurrent_commit(fixture: &RaceFixture) {
        write_file(&fixture.other.join("README.md"), "concurrent\n");
        git(&fixture.other, &["add", "README.md"]);
        git(&fixture.other, &["commit", "-m", "concurrent work"]);
        git(&fixture.other, &["push", RELEASE_REMOTE, RELEASE_BRANCH]);
    }

    #[test]
    fn failed_push_check_recognizes_a_published_release() {
        let fixture = race_fixture();
        let plan = nightly_plan(&fixture.work);
        let cargo_toml = fixture.work.join("Cargo.toml");
        let versions = plan
            .compute_versions(&cargo_toml)
            .expect("computing versions");
        let tag = versions.tag();
        let start_head = super::super::resolve_git_revision(&fixture.work, "HEAD")
            .expect("resolving initial HEAD");
        plan.create_bump_commit_and_tag(&cargo_toml, &versions)
            .expect("creating release commit and tag");
        let release_head = super::super::resolve_git_revision(&fixture.work, "HEAD")
            .expect("resolving release HEAD");
        git(&fixture.work, &[
            "push",
            "--atomic",
            RELEASE_REMOTE,
            RELEASE_BRANCH,
            tag.as_str(),
        ]);

        assert_eq!(
            plan.push_failure_disposition(&tag, &start_head, &release_head, true)
                .expect("inspecting published refs"),
            PushFailureDisposition::Published
        );
    }

    #[test]
    fn push_failure_classification_rejects_an_unchanged_remote() {
        let state = RemoteReleaseState {
            main: "start".to_string(),
            tag:  None,
        };

        assert_eq!(
            state.classify_push_failure("start", "release", true),
            PushFailureDisposition::Unchanged
        );
    }

    #[test]
    fn release_requires_the_main_branch() {
        let fixture = race_fixture();
        git(&fixture.work, &["switch", "-c", "feature"]);

        let error = nightly_plan(&fixture.work)
            .ensure_main_branch()
            .expect_err("a release from another branch should fail");

        assert!(
            format!("{error:#}").contains("release must run from main; current branch is feature"),
            "error should identify the required and current branches: {error:#}"
        );
    }

    #[test]
    fn resync_preserves_worktree_edits() {
        let fixture = race_fixture();
        let plan = nightly_plan(&fixture.work);
        let cargo_toml = fixture.work.join("Cargo.toml");
        let versions = plan
            .compute_versions(&cargo_toml)
            .expect("computing versions");
        let tag = versions.tag();
        let start_head = super::super::resolve_git_revision(&fixture.work, "HEAD")
            .expect("resolving initial HEAD");
        plan.create_bump_commit_and_tag(&cargo_toml, &versions)
            .expect("creating release commit and tag");
        let release_head = super::super::resolve_git_revision(&fixture.work, "HEAD")
            .expect("resolving release HEAD");
        write_file(&fixture.work.join("app/src/lib.rs"), "user edit\n");

        let error = plan
            .resync_with_origin_main(&tag, &start_head, &release_head)
            .expect_err("resync should stop when the worktree changes");

        assert!(
            format!("{error:#}").contains("working tree changed while the release was running"),
            "error should explain why resync stopped: {error:#}"
        );
        let diff = git(&fixture.work, &["diff", "--", "app/src/lib.rs"]);
        assert!(
            diff.contains("+user edit"),
            "resync should preserve the worktree edit:\n{diff}"
        );
    }

    #[test]
    fn resync_preserves_a_commit_created_after_the_release_commit() {
        let fixture = race_fixture();
        let plan = nightly_plan(&fixture.work);
        let cargo_toml = fixture.work.join("Cargo.toml");
        let versions = plan
            .compute_versions(&cargo_toml)
            .expect("computing versions");
        let tag = versions.tag();
        let start_head = super::super::resolve_git_revision(&fixture.work, "HEAD")
            .expect("resolving initial HEAD");
        plan.create_bump_commit_and_tag(&cargo_toml, &versions)
            .expect("creating release commit and tag");
        let release_head = super::super::resolve_git_revision(&fixture.work, "HEAD")
            .expect("resolving release HEAD");
        write_file(&fixture.work.join("local.txt"), "concurrent local work\n");
        git(&fixture.work, &["add", "local.txt"]);
        git(&fixture.work, &["commit", "-m", "concurrent local work"]);

        let error = plan
            .resync_with_origin_main(&tag, &start_head, &release_head)
            .expect_err("resync should stop when local main changes");

        assert!(
            format!("{error:#}").contains("local main changed while the release was running"),
            "error should explain why resync stopped: {error:#}"
        );
        let subject = git(&fixture.work, &["log", "-1", "--format=%s"]);
        assert_eq!(subject, "concurrent local work");
        git(&fixture.work, &[
            "rev-parse",
            "--verify",
            &ReleasePlan::tag_ref(&tag),
        ]);
    }

    #[test]
    fn push_does_not_retry_when_remote_refs_are_unchanged() {
        let fixture = race_fixture();
        write_file(&fixture.work.join("local.txt"), "unpushed local work\n");
        git(&fixture.work, &["add", "local.txt"]);
        git(&fixture.work, &["commit", "-m", "unpushed local work"]);

        let missing_push_remote = fixture
            .origin
            .parent()
            .expect("origin should have a parent")
            .join("missing.git");
        git(&fixture.work, &[
            "remote",
            "set-url",
            "--push",
            RELEASE_REMOTE,
            missing_push_remote
                .to_str()
                .expect("push remote path should be utf-8"),
        ]);

        let plan = nightly_plan(&fixture.work);
        let cargo_toml = fixture.work.join("Cargo.toml");
        let versions = plan
            .compute_versions(&cargo_toml)
            .expect("computing versions");
        plan.commit_tag_and_push(&cargo_toml, versions)
            .expect_err("an unrelated push failure should be returned");

        let subjects = git(&fixture.work, &["log", "--format=%s"]);
        assert_eq!(subjects.lines().collect::<Vec<_>>(), [
            "Bump version to 0.100.0-nightly.0",
            "unpushed local work",
            "initial"
        ]);
    }

    #[test]
    fn push_rebuilds_bump_commit_when_origin_main_moves() {
        let fixture = race_fixture();
        push_concurrent_commit(&fixture);

        let plan = nightly_plan(&fixture.work);
        let cargo_toml = fixture.work.join("Cargo.toml");
        let versions = plan
            .compute_versions(&cargo_toml)
            .expect("computing versions");
        let tag = plan
            .commit_tag_and_push(&cargo_toml, versions)
            .expect("push should rescue itself when origin/main moves");

        assert_eq!(tag, "v0.100.0-nightly.0");
        let subjects = git(&fixture.origin, &["log", "--format=%s", "main"]);
        assert_eq!(subjects.lines().collect::<Vec<_>>(), [
            "Bump version to 0.100.0-nightly.0",
            "concurrent work",
            "initial"
        ]);
        git(&fixture.origin, &[
            "rev-parse",
            "--verify",
            "refs/tags/v0.100.0-nightly.0",
        ]);
    }

    #[test]
    fn push_rebuilds_when_origin_main_was_fetched_but_not_merged() {
        let fixture = race_fixture();
        push_concurrent_commit(&fixture);
        git(&fixture.work, &["fetch", RELEASE_REMOTE, RELEASE_BRANCH]);

        let plan = nightly_plan(&fixture.work);
        let cargo_toml = fixture.work.join("Cargo.toml");
        let versions = plan
            .compute_versions(&cargo_toml)
            .expect("computing versions");
        let tag = plan
            .commit_tag_and_push(&cargo_toml, versions)
            .expect("push should rescue a local main behind its tracking branch");

        assert_eq!(tag, "v0.100.0-nightly.0");
        let subjects = git(&fixture.origin, &["log", "--format=%s", RELEASE_BRANCH]);
        assert_eq!(subjects.lines().collect::<Vec<_>>(), [
            "Bump version to 0.100.0-nightly.0",
            "concurrent work",
            "initial"
        ]);
    }

    #[test]
    fn push_recomputes_version_when_tag_is_taken() {
        let fixture = race_fixture();

        git(&fixture.other, &[
            "tag",
            "-a",
            "v0.100.0-nightly.0",
            "-m",
            "v0.100.0-nightly.0",
        ]);
        git(&fixture.other, &["push", "origin", "v0.100.0-nightly.0"]);

        let plan = nightly_plan(&fixture.work);
        let cargo_toml = fixture.work.join("Cargo.toml");
        let versions = plan
            .compute_versions(&cargo_toml)
            .expect("computing versions");
        let tag = plan
            .commit_tag_and_push(&cargo_toml, versions)
            .expect("push should rescue itself when the tag is taken");

        assert_eq!(tag, "v0.100.0-nightly.1");
        git(&fixture.origin, &[
            "rev-parse",
            "--verify",
            "refs/tags/v0.100.0-nightly.1",
        ]);
        let subject = git(&fixture.origin, &["log", "-1", "--format=%s", "main"]);
        assert_eq!(subject, "Bump version to 0.100.0-nightly.1");
    }

    #[test]
    fn push_preserves_unpushed_local_commits_on_divergence() {
        let fixture = race_fixture();

        write_file(&fixture.work.join("local.txt"), "local\n");
        git(&fixture.work, &["add", "local.txt"]);
        git(&fixture.work, &["commit", "-m", "unpushed local work"]);

        push_concurrent_commit(&fixture);

        let plan = nightly_plan(&fixture.work);
        let cargo_toml = fixture.work.join("Cargo.toml");
        let versions = plan
            .compute_versions(&cargo_toml)
            .expect("computing versions");
        let error = plan
            .commit_tag_and_push(&cargo_toml, versions)
            .expect_err("diverged local main should fail instead of being reset away");

        assert!(
            format!("{error:#}").contains("diverged"),
            "error should explain the divergence: {error:#}"
        );
        let subject = git(&fixture.work, &["log", "-1", "--format=%s"]);
        assert_eq!(subject, "unpushed local work");
    }
}
