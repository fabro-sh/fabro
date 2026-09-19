//! Fabro's [`RunSandbox`] over a sandbox-driver handle.
//!
//! Every operation goes to a public driver facet: files through
//! [`sandbox_driver::Filesystem`], content and tree search through
//! [`sandbox_driver::Search`], commands through fabro's [`SandboxExec`]
//! policy over the [`sandbox_driver::Exec`] facet, lifecycle
//! through the handle itself. Nothing here knows which provider is behind
//! the handle or whether it runs in-process or over the plugin wire.
//!
//! What stays fabro's: the exec ladder and the run-facing conventions
//! (`platform` names, grep line format, walk results relative to a
//! caller-declared base). The driver reports lifecycle events itself,
//! through the [`EventContext`] a sandbox is created or attached with.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use fabro_github::GitHubCredentials;
use fabro_github::token_source::TokenSnapshot;
use fabro_types::SandboxProviderKind;
use fabro_util::workspace_glob::WorkspaceGlob;
use pebble_coding_agent::mcp::{PortRoute, PortRouteError, PortRoutes};
use sandbox_driver::{
    DirEntry, EventContext, ExecControls, ExecResult, ExecSpec, ExecStreamingResult, FileKind,
    GitRetryPolicy, GrepMatch, GrepOptions, PreviewUrls, PtyOptions, PtySession, PtySize,
    Sandbox as DriverHandle, SandboxProvider as DriverProvider, SandboxSpec as DriverSpec,
    SandboxState, Search as _, StdioProcess, WaitOptions, WalkOptions,
};
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use crate::clone::{self, GitHubClone};
use crate::clone_source::{self, CloneDecision, EmptyWorkspaceReason};
use crate::credentials::{self, RepoCredentials};
use crate::environment::CloneRequest;
use crate::exec::SandboxExec;
use crate::sandbox::{self, PushError, PushReport, SandboxFile, SandboxWorkspaceLayout};
use crate::{GitRunInfo, GitSetupIntent};

/// Where a clone-based provider puts its files: the run works under
/// `workspace_root`, and repositories check out under `repos_root`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceLayout {
    pub(crate) workspace_root: String,
    pub(crate) repos_root:     String,
}

impl WorkspaceLayout {
    /// The layout for a provider whose working directory fabro does not
    /// choose: repositories check out beside the workspace contents under
    /// `.repos`, and the run works in the link the workspace root carries.
    pub(crate) fn within(working_directory: &str) -> Self {
        Self {
            workspace_root: working_directory.to_string(),
            repos_root:     sandbox::join_sandbox_path(working_directory, ".repos"),
        }
    }
}

/// How a workspace learns its layout.
pub(crate) enum LayoutSource {
    /// Fabro fixes the roots before the sandbox exists.
    Fixed(WorkspaceLayout),
    /// The roots follow the provider's working directory, known once the
    /// sandbox exists.
    ProviderWorkingDirectory,
}

/// What `initialize` does to the workspace once the sandbox runs.
enum WorkspacePlan {
    /// Clone this GitHub repository into the layout.
    Clone(GitHubClone),
    /// Create the empty workspace root and nothing else.
    Empty(EmptyWorkspaceReason),
    /// The workspace was prepared by an earlier process; leave it alone.
    Attached,
}

/// The run's workspace on a sandbox: the layout, the clone fabro performs
/// into it (if any), and the GitHub credentials its checkout carries. A
/// workspace fabro did not clone into is still a checkout the run may push
/// from, with whatever credentials the checkout carries itself.
pub(crate) struct RepoWorkspace {
    layout:              OnceLock<WorkspaceLayout>,
    plan:                WorkspacePlan,
    credentials:         RepoCredentials,
    repo_cloned:         OnceLock<bool>,
    origin_url:          OnceLock<String>,
    /// The directory the run works in once known: the repository link for a
    /// clone, the workspace root otherwise.
    execution_directory: OnceLock<String>,
    /// The real checkout behind the workspace link, for traversals that
    /// must not start at a symlink.
    checkout_path:       OnceLock<String>,
}

impl RepoWorkspace {
    /// Decide the clone for a new sandbox. Fails before any provider call
    /// when the selectors are inconsistent (a pin without a branch, a
    /// non-GitHub origin without `skip`).
    pub(crate) fn plan(
        layout: LayoutSource,
        clone: &CloneRequest,
        github_app: Option<&GitHubCredentials>,
    ) -> crate::Result<Self> {
        let decision = clone_source::decide_clone(
            clone.skip,
            clone.origin_url.as_deref(),
            clone.branch.as_deref(),
            clone.tag.as_deref(),
            clone.commit_sha.as_deref(),
        )?;
        let credentials = RepoCredentials::new(credentials::build_token_source(
            github_app,
            clone.origin_url.as_deref(),
        )?);
        let plan = match decision {
            CloneDecision::EmptyWorkspace { reason } => WorkspacePlan::Empty(reason),
            CloneDecision::GitHub {
                origin_url,
                branch,
                tag,
                commit_sha,
            } => WorkspacePlan::Clone(GitHubClone {
                origin_url,
                branch,
                tag,
                commit_sha,
                depth: clone.depth,
            }),
        };
        Ok(Self {
            layout: layout.into_cell(),
            plan,
            credentials,
            repo_cloned: OnceLock::new(),
            origin_url: OnceLock::new(),
            execution_directory: OnceLock::new(),
            checkout_path: OnceLock::new(),
        })
    }

    /// A workspace prepared by an earlier process, described by the run
    /// record. Pushes from a reattached sandbox use whatever credentials the
    /// checkout's credential store already carries.
    pub(crate) fn attached(
        layout: LayoutSource,
        repo_cloned: bool,
        working_directory: String,
        clone_origin_url: Option<String>,
    ) -> Self {
        let workspace = Self {
            layout:              layout.into_cell(),
            plan:                WorkspacePlan::Attached,
            credentials:         RepoCredentials::none(),
            repo_cloned:         OnceLock::new(),
            origin_url:          OnceLock::new(),
            execution_directory: OnceLock::new(),
            checkout_path:       OnceLock::new(),
        };
        let _ = workspace.repo_cloned.set(repo_cloned);
        let _ = workspace.execution_directory.set(working_directory);
        if repo_cloned {
            if let Some(origin) = clone_origin_url {
                let _ = workspace.origin_url.set(origin);
            }
        }
        workspace.derive_checkout_path();
        workspace
    }

    /// The workspace an existing handle already works in, whatever it
    /// holds: nothing fabro cloned, laid out from the handle's own working
    /// directory.
    pub(crate) fn existing() -> Self {
        Self {
            layout:              LayoutSource::ProviderWorkingDirectory.into_cell(),
            plan:                WorkspacePlan::Attached,
            credentials:         RepoCredentials::none(),
            repo_cloned:         OnceLock::new(),
            origin_url:          OnceLock::new(),
            execution_directory: OnceLock::new(),
            checkout_path:       OnceLock::new(),
        }
    }

    /// Settle a provider-dependent layout from the sandbox's working
    /// directory. A fixed layout is left alone.
    fn resolve_layout(&self, provider_working_directory: &str) -> &WorkspaceLayout {
        let layout = self
            .layout
            .get_or_init(|| WorkspaceLayout::within(provider_working_directory));
        self.derive_checkout_path();
        layout
    }

    /// The checkout behind an attached clone, once the layout is known.
    fn derive_checkout_path(&self) {
        if self.checkout_path.get().is_some() || !self.repo_cloned() {
            return;
        }
        let (Some(layout), Some(origin)) = (self.layout.get(), self.origin_url.get()) else {
            return;
        };
        if let Ok(repo_layout) =
            clone_source::github_repo_layout(origin, &layout.workspace_root, &layout.repos_root)
        {
            let _ = self.checkout_path.set(repo_layout.primary_repo_path);
        }
    }

    fn repo_cloned(&self) -> bool {
        self.repo_cloned.get().copied().unwrap_or(false)
    }

    fn working_directory(&self) -> Option<&str> {
        self.execution_directory
            .get()
            .map(String::as_str)
            .or_else(|| {
                self.layout
                    .get()
                    .map(|layout| layout.workspace_root.as_str())
            })
    }

    fn record(&self) -> Option<SandboxWorkspaceLayout> {
        let layout = self.layout.get()?;
        let repo = if self.repo_cloned() {
            self.origin_url.get().and_then(|origin| {
                clone_source::github_repo_layout(origin, &layout.workspace_root, &layout.repos_root)
                    .ok()
            })
        } else {
            None
        };
        Some(SandboxWorkspaceLayout {
            workspace_root:    layout.workspace_root.clone(),
            repos_root:        layout.repos_root.clone(),
            primary_repo_path: repo.as_ref().map(|repo| repo.primary_repo_path.clone()),
            primary_repo_link: repo.as_ref().map(|repo| repo.primary_repo_link.clone()),
        })
    }
}

impl LayoutSource {
    fn into_cell(self) -> OnceLock<WorkspaceLayout> {
        let cell = OnceLock::new();
        if let Self::Fixed(layout) = self {
            let _ = cell.set(layout);
        }
        cell
    }
}

/// A sandbox that does not exist yet: `initialize` creates it on the
/// provider from `spec`.
struct PendingCreate {
    provider: Arc<dyn DriverProvider>,
    spec:     DriverSpec,
}

/// A fabro sandbox backed by a sandbox-driver handle.
pub struct RunSandbox {
    kind:      SandboxProviderKind,
    /// Set at construction for an existing sandbox, at `initialize` for a
    /// pending one.
    handle:    OnceCell<Arc<dyn DriverHandle>>,
    pending:   Option<PendingCreate>,
    workspace: RepoWorkspace,
    /// Where the driver reports the lifecycle of a sandbox this creates.
    /// Set before `initialize` on a pending sandbox; an existing handle
    /// already carries the context it was created or attached with.
    events:    Option<EventContext>,
    /// `(platform, os_version)` learned from the sandbox at initialize or
    /// start; unknown until then.
    platform:  OnceLock<(String, String)>,
    /// The provider snapshot the sandbox was created from, when known.
    snapshot:  OnceLock<String>,
}

impl RunSandbox {
    /// Wraps an existing driver handle as a sandbox of `kind`, working in
    /// whatever the handle's working directory holds.
    #[must_use]
    pub fn new(kind: SandboxProviderKind, handle: Arc<dyn DriverHandle>) -> Self {
        Self::attached(kind, handle, RepoWorkspace::existing())
    }

    /// A sandbox over an existing handle whose platform is already known,
    /// so tests need no activation round trip before reading it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn new_with_platform(
        kind: SandboxProviderKind,
        handle: Arc<dyn DriverHandle>,
        platform: impl Into<String>,
        os_version: impl Into<String>,
    ) -> Self {
        let sandbox = Self::new(kind, handle);
        let _ = sandbox.platform.set((platform.into(), os_version.into()));
        sandbox
    }

    /// A sandbox `initialize` will create from `spec` on `provider`, then
    /// prepare per `workspace`.
    pub(crate) fn pending(
        kind: SandboxProviderKind,
        provider: Arc<dyn DriverProvider>,
        spec: DriverSpec,
        workspace: RepoWorkspace,
    ) -> Self {
        let mut sandbox = Self::empty(kind, workspace);
        sandbox.pending = Some(PendingCreate { provider, spec });
        sandbox
    }

    /// Records the provider snapshot an attached sandbox was created from.
    pub(crate) fn set_snapshot(&self, snapshot: String) {
        let _ = self.snapshot.set(snapshot);
    }

    /// An existing sandbox reattached by handle, with the workspace an
    /// earlier process prepared.
    pub(crate) fn attached(
        kind: SandboxProviderKind,
        handle: Arc<dyn DriverHandle>,
        workspace: RepoWorkspace,
    ) -> Self {
        workspace.resolve_layout(handle.working_directory());
        let sandbox = Self::empty(kind, workspace);
        let _ = sandbox.handle.set(handle);
        sandbox
    }

    fn empty(kind: SandboxProviderKind, workspace: RepoWorkspace) -> Self {
        Self {
            kind,
            handle: OnceCell::new(),
            pending: None,
            workspace,
            events: None,
            platform: OnceLock::new(),
            snapshot: OnceLock::new(),
        }
    }

    /// Where the driver reports this sandbox's lifecycle once `initialize`
    /// creates it. An existing handle reports through the context it was
    /// created or attached with, so this only matters for a pending sandbox.
    pub fn set_events(&mut self, events: EventContext) {
        self.events = Some(events);
    }

    /// The provider kind fabro persists for this sandbox.
    #[must_use]
    pub fn kind(&self) -> &SandboxProviderKind {
        &self.kind
    }

    /// The driver handle, for callers that need a facet fabro's trait does
    /// not carry (git, services, access). Absent until a pending sandbox is
    /// initialized.
    pub fn handle(&self) -> crate::Result<&Arc<dyn DriverHandle>> {
        self.handle.get().ok_or_else(|| {
            crate::Error::message(format!(
                "{} sandbox is not initialized; call initialize() first",
                self.kind
            ))
        })
    }

    /// Fabro's exec policy over the driver's exec facet, working in the
    /// run's directory. Absent until a pending sandbox is initialized.
    pub fn exec(&self) -> crate::Result<SandboxExec<'_>> {
        let mut exec = SandboxExec::new(self.handle()?.exec());
        if let Some(dir) = self.workspace.execution_directory.get() {
            exec = exec.with_working_dir(dir.clone());
        }
        Ok(exec)
    }

    /// Resolve a caller path against fabro's working directory. The driver
    /// resolves relative paths against the sandbox's own working directory,
    /// which sits above a cloned repository's link.
    fn resolve(&self, path: &str) -> String {
        match self.workspace.execution_directory.get() {
            Some(working_directory) => sandbox::resolve_path(path, working_directory),
            None => path.to_string(),
        }
    }

    /// The driver's git facet for this sandbox's checkout, for fabro's own
    /// git operations (checkpoints, diffs, the Run Files listing). Absent
    /// until a pending sandbox is initialized, or when the provider has no
    /// git. Pass [`Self::working_directory`] as the repository path.
    pub fn git(&self) -> crate::Result<sandbox_driver::GitFacet<'_>> {
        self.handle()?.git().ok_or_else(|| {
            crate::Error::message(format!(
                "sandbox provider `{}` does not support git",
                self.kind
            ))
        })
    }

    /// The driver's services facet for this sandbox: background processes
    /// that outlive their exec (the agent's MCP servers, dev servers), the
    /// wait for a port to answer, and the list of listeners. Absent until a
    /// pending sandbox is initialized, or when the provider has no
    /// services.
    pub fn services(&self) -> crate::Result<sandbox_driver::ServicesFacet<'_>> {
        self.handle()?.services().ok_or_else(|| {
            crate::Error::message(format!(
                "sandbox provider `{}` does not support background services",
                self.kind
            ))
        })
    }

    fn search(&self) -> crate::Result<sandbox_driver::SearchFacet<'_>> {
        self.handle()?.search().ok_or_else(|| {
            crate::Error::message(format!(
                "sandbox provider `{}` does not support search",
                self.kind
            ))
        })
    }

    /// Create the sandbox on the provider when it does not exist yet.
    async fn ensure_created(&self) -> crate::Result<()> {
        if self.handle.get().is_some() {
            return Ok(());
        }
        let Some(pending) = &self.pending else {
            return self.handle().map(|_| ());
        };
        let handle = pending
            .provider
            .create(&pending.spec, self.events.clone())
            .await
            .map_err(|error| {
                crate::Error::context(format!("Failed to create {} sandbox", self.kind), error)
            })?;
        // The provider may have created the sandbox from a snapshot it
        // built or chose (Daytona caches images as snapshots); the run
        // record names it.
        if let Ok(status) = handle.describe().await {
            if let Some(snapshot) = status.snapshot {
                let _ = self.snapshot.set(snapshot);
            }
        }
        let _ = self.handle.set(handle);
        Ok(())
    }

    /// Bring the sandbox to `Running` with a verified Bash, and learn its
    /// platform. Shared by initialize and activate.
    async fn make_ready(&self) -> crate::Result<()> {
        sandbox_driver::activate(self.handle()?.as_ref(), &WaitOptions::default()).await?;
        self.learn_platform().await
    }

    /// Prepare the workspace after the sandbox runs for the first time:
    /// an empty root, or fabro's clone.
    async fn prepare_workspace(&self) -> crate::Result<()> {
        let workspace = &self.workspace;
        let layout = workspace
            .resolve_layout(self.handle()?.working_directory())
            .clone();
        match &workspace.plan {
            WorkspacePlan::Attached => Ok(()),
            WorkspacePlan::Empty(reason) => {
                if matches!(reason, EmptyWorkspaceReason::MissingOrigin) {
                    tracing::warn!(
                        provider = %self.kind,
                        reason = reason.message(),
                        "Clone source missing for clone-based sandbox"
                    );
                }
                self.handle()?
                    .fs()
                    .create_dir(&layout.workspace_root)
                    .await
                    .map_err(|error| {
                        crate::Error::context(
                            format!("Failed to create {}", layout.workspace_root),
                            error,
                        )
                    })?;
                let _ = workspace.repo_cloned.set(false);
                let _ = workspace
                    .execution_directory
                    .set(layout.workspace_root.clone());
                Ok(())
            }
            WorkspacePlan::Clone(plan) => {
                tracing::debug!(
                    url = plan.origin_url.as_str(),
                    branch = plan.branch.as_deref().unwrap_or(""),
                    "Git clone started"
                );
                let started = Instant::now();
                let handle = self.handle()?;
                // The clone names every directory it touches, so it runs
                // without fabro's working-directory override.
                let exec = SandboxExec::new(handle.exec());
                let outcome = clone::clone_github_repo(
                    &self.kind,
                    handle.as_ref(),
                    &exec,
                    plan,
                    &layout.workspace_root,
                    &layout.repos_root,
                    &workspace.credentials,
                )
                .await;
                match outcome {
                    Ok(outcome) => {
                        let _ = workspace.repo_cloned.set(true);
                        let _ = workspace.origin_url.set(plan.origin_url.clone());
                        let _ = workspace
                            .checkout_path
                            .set(outcome.layout.primary_repo_path.clone());
                        let _ = workspace
                            .execution_directory
                            .set(outcome.layout.execution_directory.clone());
                        tracing::debug!(
                            url = plan.origin_url.as_str(),
                            duration_ms = elapsed_ms(started),
                            "Git clone completed"
                        );
                        Ok(())
                    }
                    Err(error) => {
                        tracing::error!(
                            url = plan.origin_url.as_str(),
                            error = %error,
                            causes = ?error.causes(),
                            "Git clone failed"
                        );
                        Err(error)
                    }
                }
            }
        }
    }

    /// Open an interactive shell in the sandbox's working directory over the
    /// driver's Pty facet.
    pub async fn open_terminal(&self, size: PtySize) -> crate::Result<Box<dyn PtySession>> {
        let handle = self.handle()?;
        let pty = handle.pty().ok_or_else(|| {
            crate::Error::message(format!(
                "sandbox provider `{}` does not support terminals",
                self.kind
            ))
        })?;
        let mut options = PtyOptions::default();
        options.size = size;
        options.working_dir = Some(self.working_directory().to_string());
        pty.open(&options)
            .await
            .map_err(|error| crate::Error::context("Failed to open sandbox terminal", error))
    }

    /// Ask the sandbox for its platform once; `platform` and `os_version`
    /// report `unknown` until this has run.
    async fn learn_platform(&self) -> crate::Result<()> {
        if self.platform.get().is_none() {
            let info = self.handle()?.platform_info().await?;
            let platform = fabro_platform_name(&info.os).to_string();
            let os_version = if info.version.is_empty() {
                platform.clone()
            } else {
                format!("{platform} {}", info.version)
            };
            let _ = self.platform.set((platform, os_version));
        }
        Ok(())
    }

    /// The traversal base the driver walks. A base at the sandbox working
    /// directory walks relative to it so every path component of
    /// `relative_start` is checked against symlinks; any other base is
    /// walked as given.
    fn walk_base(&self, base: &str, relative_start: &str) -> String {
        if base == self.working_directory() || base.is_empty() || base == "." {
            // A cloned repository is reached through a workspace link. The
            // driver refuses a symlinked traversal root, so walk the real
            // checkout; results are reported under the link.
            if let Some(checkout) = self.workspace.checkout_path.get() {
                return sandbox::join_sandbox_path(checkout, relative_start);
            }
            if relative_start.is_empty() {
                ".".to_string()
            } else {
                relative_start.to_string()
            }
        } else {
            sandbox::join_sandbox_path(&self.resolve(base), relative_start)
        }
    }
}

/// Fabro names the macOS platform `darwin`, as `uname -s` does.
fn fabro_platform_name(os: &str) -> &str {
    match os {
        "macos" => "darwin",
        other => other,
    }
}

fn file_context(action: &str, path: &str) -> String {
    format!("Failed to {action} {path}")
}

impl RunSandbox {
    pub async fn read_file_bytes(&self, path: &str) -> crate::Result<Vec<u8>> {
        self.handle()?
            .fs()
            .read(&self.resolve(path))
            .await
            .map_err(|error| crate::Error::context(file_context("read", path), error))
    }

    pub async fn read_file_text(&self, path: &str) -> crate::Result<String> {
        String::from_utf8(self.read_file_bytes(path).await?)
            .map_err(|err| crate::Error::context("File is not valid UTF-8", err))
    }

    pub async fn write_file(&self, path: &str, content: &str) -> crate::Result<()> {
        self.handle()?
            .fs()
            .write(&self.resolve(path), content.as_bytes())
            .await
            .map_err(|error| crate::Error::context(file_context("write", path), error))
    }

    pub async fn delete_file(&self, path: &str) -> crate::Result<()> {
        // Fabro's contract fails on a missing file; the driver's delete is
        // idempotent, so check first.
        if !self.file_exists(path).await? {
            return Err(crate::Error::message(format!(
                "{}: file does not exist",
                file_context("delete", path)
            )));
        }
        self.handle()?
            .fs()
            .delete(&self.resolve(path), false)
            .await
            .map_err(|error| crate::Error::context(file_context("delete", path), error))
    }

    pub async fn file_exists(&self, path: &str) -> crate::Result<bool> {
        self.handle()?
            .fs()
            .exists(&self.resolve(path))
            .await
            .map_err(|error| crate::Error::context(file_context("stat", path), error))
    }

    /// Lists a directory to `depth` (`None` is the immediate children),
    /// sorted by path. Sizes are reported for files only.
    pub async fn list_directory(
        &self,
        path: &str,
        depth: Option<usize>,
    ) -> crate::Result<Vec<DirEntry>> {
        let mut entries = self
            .handle()?
            .fs()
            .list_dir(&self.resolve(path), depth.unwrap_or(1))
            .await
            .map_err(|error| crate::Error::context(file_context("list", path), error))?;
        for entry in &mut entries {
            if entry.kind != FileKind::File {
                entry.size = None;
            }
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(entries)
    }

    pub async fn exec_command(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
    ) -> crate::Result<ExecResult> {
        self.exec()?
            .run(
                command,
                Some(Duration::from_millis(timeout_ms)),
                working_dir,
                env_vars,
                cancel_token,
            )
            .await
    }

    /// Runs `spec` under fabro's exec policy, delivering output through
    /// `controls.sink` as it arrives. Build the spec with
    /// [`ExecSpec::bash`]; the policy fills the stop grace, the run's
    /// working directory, and the environment filter where the spec leaves
    /// them open.
    pub async fn exec_command_streaming(
        &self,
        spec: ExecSpec,
        controls: ExecControls,
    ) -> crate::Result<ExecStreamingResult> {
        self.exec()?.run_streaming(spec, controls).await
    }

    /// Launches a long-lived process with bidirectional stdio. The returned
    /// handle terminates the process; dropping it does not.
    pub async fn spawn_stdio_process(
        &self,
        command: &str,
        working_dir: Option<&str>,
        env_vars: Option<&HashMap<String, String>>,
    ) -> crate::Result<StdioProcess> {
        self.exec()?
            .spawn_stdio(command, working_dir, env_vars)
            .await
    }

    /// Searches file contents below `path`, resolved against the run's
    /// working directory.
    pub async fn grep(
        &self,
        pattern: &str,
        path: &str,
        options: &GrepOptions,
    ) -> crate::Result<Vec<GrepMatch>> {
        self.search()?
            .grep(pattern, &self.resolve(path), options)
            .await
            .map_err(|error| crate::Error::context("Failed to search file contents", error))
    }

    /// Recursively enumerates regular files below `base`, starting at the
    /// literal directory `relative_start` inside it. Every returned
    /// `relative_path` is relative to `base`; `options.exclude_dirs` names
    /// directory basenames pruned at every depth, including on the way to
    /// `relative_start`.
    pub async fn walk_files(
        &self,
        base: &str,
        relative_start: &str,
        options: &WalkOptions,
    ) -> crate::Result<Vec<SandboxFile>> {
        if relative_start.split('/').any(|segment| {
            options
                .exclude_dirs
                .iter()
                .any(|excluded| excluded == segment)
        }) {
            return Ok(Vec::new());
        }
        let walk_base = self.walk_base(base, relative_start);
        let walked = self
            .search()?
            .walk(&walk_base, options)
            .await
            .map_err(|error| crate::Error::context("Failed to enumerate files", error))?;
        let mut files = Vec::with_capacity(walked.len());
        for file in walked {
            let relative_path = sandbox::join_sandbox_path(relative_start, &file.path);
            let path = sandbox::join_sandbox_path(base, &relative_path);
            // A transport without sizes (BSD `find`) reports `None`; fabro's
            // callers budget by size, so ask the filesystem rather than guess.
            let size = match file.size {
                Some(size) => size,
                None => {
                    self.handle()?
                        .fs()
                        .metadata(&path)
                        .await
                        .map_err(|error| crate::Error::context(file_context("stat", &path), error))?
                        .size
                }
            };
            files.push(SandboxFile {
                path,
                relative_path,
                size,
            });
        }
        Ok(files)
    }

    /// Matches a workspace-relative glob with provider-independent
    /// semantics, over [`RunSandbox::walk_files`].
    pub async fn glob(&self, pattern: &str, path: Option<&str>) -> crate::Result<Vec<String>> {
        let glob = WorkspaceGlob::try_new(pattern)
            .map_err(|error| crate::Error::context("Invalid glob pattern", error))?;
        let base = path.unwrap_or_else(|| self.working_directory());
        let mut files = self
            .walk_files(base, glob.traversal_root(), &WalkOptions::default())
            .await?
            .into_iter()
            .filter(|file| glob.is_match(&file.relative_path))
            .collect::<Vec<_>>();
        files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        Ok(files.into_iter().map(|file| file.path).collect())
    }

    pub async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> crate::Result<()> {
        self.handle()?
            .fs()
            .download(&self.resolve(remote_path), local_path)
            .await
            .map_err(|error| crate::Error::context(file_context("download", remote_path), error))
    }

    pub async fn upload_file_from_local(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> crate::Result<()> {
        self.handle()?
            .fs()
            .upload(local_path, &self.resolve(remote_path))
            .await
            .map_err(|error| crate::Error::context(file_context("upload", remote_path), error))
    }

    /// Create the sandbox when it is pending, bring it to `Running`, and
    /// prepare fabro's workspace (empty root or clone) on first use.
    pub async fn initialize(&self) -> crate::Result<()> {
        self.ensure_created().await?;
        self.make_ready().await?;
        self.prepare_workspace().await
    }

    /// The provider's console page for this sandbox, when it has one. Best
    /// effort: a failed describe reports no page.
    pub async fn console_url(&self) -> Option<String> {
        self.handle()
            .ok()?
            .describe()
            .await
            .ok()
            .and_then(|status| status.web_url)
    }

    /// Brings the sandbox back into use, idempotently: a running sandbox is
    /// left alone and only its platform is learned when unknown; a stopped
    /// or paused one is started and its Bash verified. Resume and every
    /// access-time caller share this one entry point.
    pub async fn activate(&self) -> crate::Result<()> {
        let status = self.handle()?.describe().await?;
        if status.state == SandboxState::Running {
            return self.learn_platform().await;
        }
        self.make_ready().await
    }

    pub async fn stop(&self) -> crate::Result<()> {
        self.handle()?.stop().await.map_err(crate::Error::from)
    }

    /// Releases the sandbox. For a designated host directory this frees the
    /// handle and leaves the directory in place; for an isolated provider it
    /// removes the sandbox. A pending sandbox that was never created has
    /// nothing to release.
    pub async fn delete(&self) -> crate::Result<()> {
        self.release().await
    }

    /// The directory the run works in: the cloned repository's link for a
    /// clone-based workspace, the provider's working directory otherwise.
    pub fn working_directory(&self) -> &str {
        self.workspace
            .working_directory()
            .or_else(|| self.handle.get().map(|handle| handle.working_directory()))
            .unwrap_or("")
    }

    pub fn runtime_directory(&self) -> Option<&str> {
        self.handle
            .get()
            .and_then(|handle| handle.runtime_directory())
    }

    pub fn platform(&self) -> &str {
        self.platform
            .get()
            .map_or("unknown", |(platform, _)| platform.as_str())
    }

    pub fn os_version(&self) -> String {
        self.platform.get().map_or_else(
            || self.platform().to_string(),
            |(_, version)| version.clone(),
        )
    }

    /// The provider's id for this sandbox; for `local`, the id the Host
    /// provider derives from the working directory. Empty for a pending
    /// sandbox that has not been created.
    pub fn sandbox_info(&self) -> String {
        self.handle
            .get()
            .map(|handle| handle.id().to_string())
            .unwrap_or_default()
    }

    pub fn snapshot_info(&self) -> Option<String> {
        self.snapshot.get().cloned()
    }

    pub fn workspace_layout(&self) -> Option<SandboxWorkspaceLayout> {
        self.workspace.record()
    }

    pub async fn setup_git(&self, intent: &GitSetupIntent) -> crate::Result<Option<GitRunInfo>> {
        if !self.repo_cloned() {
            return Ok(None);
        }
        sandbox::setup_git(self, intent).await.map(Some)
    }

    /// Push `refspec` from the run's checkout. A checkout fabro cloned
    /// pushes with the credentials it was cloned with. Any other checkout
    /// pushes only when it has an origin, with whatever credentials it
    /// carries itself; a workspace without one has nothing to push.
    pub async fn git_push_ref(
        &self,
        refspec: &str,
        policy: &GitRetryPolicy,
    ) -> Result<PushReport, PushError> {
        let workspace = &self.workspace;
        if workspace.repo_cloned() {
            return sandbox::git_push(self, Some(&workspace.credentials), refspec, policy).await;
        }
        let has_origin = match self
            .exec_command("git remote get-url origin", 10_000, None, None, None)
            .await
        {
            Ok(result) => result.success(),
            Err(err) => {
                return Err(PushError {
                    report: PushReport::default(),
                    error:  crate::Error::context("git remote get-url origin", err),
                });
            }
        };
        if !has_origin {
            return Ok(PushReport::default());
        }
        sandbox::git_push(self, None, refspec, policy).await
    }

    pub fn origin_url(&self) -> Option<&str> {
        if !self.workspace.repo_cloned() {
            return None;
        }
        self.workspace.origin_url.get().map(String::as_str)
    }

    /// Renew the credentials the agent's own git commands read for the
    /// checkout: resolve the current token and rewrite the checkout's
    /// credential store with it. Returns the token's non-secret description,
    /// or `None` when this sandbox has no managed credentials or no
    /// checkout to install them in.
    #[tracing::instrument(name = "git_op", skip_all, fields(op = "refresh-credentials"))]
    pub async fn refresh_ambient_credentials(&self) -> crate::Result<Option<TokenSnapshot>> {
        let workspace = &self.workspace;
        let Some(checkout) = workspace.checkout_path.get() else {
            return Ok(None);
        };
        let Some(token) = workspace.credentials.resolve().await? else {
            return Ok(None);
        };
        RepoCredentials::install(&self.git()?, checkout, &token).await?;
        Ok(Some(token.snapshot))
    }

    /// The local command that opens a shell in the sandbox, from the
    /// provider's access facet. `None` when the provider has no such
    /// command (the local sandbox is the host).
    pub async fn ssh_access_command(&self) -> crate::Result<Option<String>> {
        let Some(shell) = self.handle()?.shell_command() else {
            return Ok(None);
        };
        shell
            .shell_command()
            .await
            .map(Some)
            .map_err(|error| crate::Error::context("Failed to build sandbox shell command", error))
    }

    /// The route from fabro to a port inside the sandbox, as pebble's MCP
    /// support takes it: pebble's [`PortRoutes`] over the driver's preview
    /// URLs, when the provider has them. `None` for a provider without
    /// forwarding, which is where pebble reaches the port on the loopback
    /// address instead.
    #[must_use]
    pub fn port_routes(self: &Arc<Self>) -> Option<Arc<dyn PortRoutes>> {
        self.handle().ok()?.preview_urls()?;
        Some(Arc::new(SandboxPortRoutes(Arc::clone(self))))
    }

    pub async fn get_preview_url(
        &self,
        port: u16,
    ) -> crate::Result<Option<(String, HashMap<String, String>)>> {
        let Some(previews) = self.handle()?.preview_urls() else {
            return Ok(None);
        };
        let preview = previews
            .preview_url(port)
            .await
            .map_err(|error| crate::Error::context("Failed to obtain a preview URL", error))?;
        Ok(Some((
            preview.url,
            preview.headers.into_iter().collect::<HashMap<_, _>>(),
        )))
    }
}

/// Pebble's [`PortRoutes`] over a run sandbox's driver handle: the driver's
/// preview-URL facet answers with the URL and headers that reach a port.
struct SandboxPortRoutes(Arc<RunSandbox>);

impl SandboxPortRoutes {
    /// The driver's facet, present whenever [`RunSandbox::port_routes`] handed
    /// this out: the handle is set once and never cleared. A missing facet is
    /// the environment routing to none of its ports.
    fn facet(&self) -> Result<&dyn PreviewUrls, PortRouteError> {
        self.0
            .handle()
            .ok()
            .and_then(|handle| handle.preview_urls())
            .ok_or(PortRouteError::Unsupported)
    }
}

#[async_trait::async_trait]
impl PortRoutes for SandboxPortRoutes {
    async fn route(&self, port: u16) -> Result<PortRoute, PortRouteError> {
        let preview = self.facet()?.preview_url(port).await.map_err(|error| {
            PortRouteError::failed_with_source(
                format!("Failed to open a route to sandbox port {port}"),
                error,
            )
        })?;
        Ok(PortRoute {
            url:     preview.url,
            headers: preview.headers,
        })
    }

    async fn release(&self, port: u16) -> Result<(), PortRouteError> {
        self.facet()?
            .release_preview_url(port)
            .await
            .map_err(|error| {
                PortRouteError::failed_with_source(
                    format!("Failed to release the route to sandbox port {port}"),
                    error,
                )
            })
    }
}

impl RunSandbox {
    fn repo_cloned(&self) -> bool {
        self.workspace.repo_cloned()
    }

    /// Delete the sandbox on the provider. A pending sandbox that was never
    /// created has nothing to release.
    async fn release(&self) -> crate::Result<()> {
        match self.handle.get() {
            Some(handle) => handle.delete().await.map_err(crate::Error::from),
            None if self.pending.is_some() => Ok(()),
            None => self.handle().map(|_| ()),
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use sandbox_driver::{SandboxProvider as _, SandboxSource, SandboxSpec, Termination};
    use sandbox_driver_host::HostProvider;
    use tokio::fs;

    use super::*;
    use crate::driver::ProviderAccess;
    use crate::exec::ExecResultExt;
    use crate::provider_sandbox::local_sandbox;
    use crate::sandbox_spec::SandboxSpec as RunSandboxSpec;

    struct Fixture {
        dir:       tempfile::TempDir,
        _provider: HostProvider,
        sandbox:   RunSandbox,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let provider = HostProvider::new();
        let handle = provider
            .create(
                &SandboxSpec::new(SandboxSource::HostDirectory)
                    .working_directory(dir.path().display().to_string()),
                None,
            )
            .await
            .unwrap();
        Fixture {
            dir,
            _provider: provider,
            sandbox: RunSandbox::new(SandboxProviderKind::LOCAL, handle),
        }
    }

    #[tokio::test]
    async fn files_round_trip_through_the_filesystem_facet() {
        let f = fixture().await;
        f.sandbox
            .write_file("sub/dir/test.txt", "content")
            .await
            .unwrap();
        assert!(f.dir.path().join("sub/dir/test.txt").is_file());
        assert_eq!(
            f.sandbox.read_file_text("sub/dir/test.txt").await.unwrap(),
            "content"
        );
        assert!(f.sandbox.file_exists("sub/dir/test.txt").await.unwrap());
        f.sandbox.delete_file("sub/dir/test.txt").await.unwrap();
        assert!(!f.sandbox.file_exists("sub/dir/test.txt").await.unwrap());
        let missing = f.sandbox.delete_file("sub/dir/test.txt").await.unwrap_err();
        assert!(missing.to_string().contains("does not exist"), "{missing}");
        let read = f
            .sandbox
            .read_file_text("nonexistent.txt")
            .await
            .unwrap_err();
        assert!(
            matches!(read.driver(), Some(sandbox_driver::Error::NotFound { .. })),
            "{read}"
        );
    }

    #[tokio::test]
    async fn list_directory_is_sorted_with_sizes_for_files_only() {
        let f = fixture().await;
        fs::write(f.dir.path().join("b.txt"), "b").await.unwrap();
        fs::write(f.dir.path().join("a.txt"), "aa").await.unwrap();
        fs::create_dir(f.dir.path().join("c_dir")).await.unwrap();
        fs::write(f.dir.path().join("c_dir/inner.txt"), "x")
            .await
            .unwrap();

        let entries = f.sandbox.list_directory(".", None).await.unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "c_dir"]);
        assert_eq!(entries[0].size, Some(2));
        assert_eq!(entries[0].kind, FileKind::File);
        assert_eq!(entries[2].kind, FileKind::Directory);
        assert_eq!(entries[2].size, None);

        let deep = f.sandbox.list_directory(".", Some(2)).await.unwrap();
        assert!(deep.iter().any(|e| e.path == "c_dir/inner.txt"));
    }

    #[tokio::test]
    async fn exec_runs_bash_with_fabro_termination_semantics() {
        let f = fixture().await;
        let ok = f
            .sandbox
            .exec_command(
                "echo hello; [[ 1 == 1 ]] && echo bash",
                5000,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(ok.stdout_lossy(), "hello\nbash\n");
        assert!(ok.success());
        let timed_out = f
            .sandbox
            .exec_command("sleep 10", 200, None, None, None)
            .await
            .unwrap();
        assert_eq!(timed_out.termination, Termination::TimedOut);
        assert_eq!(timed_out.program_exit_code(), None);
    }

    #[tokio::test]
    async fn grep_returns_path_line_content_triples() {
        let f = fixture().await;
        fs::write(
            f.dir.path().join("test.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .await
        .unwrap();
        let results = f
            .sandbox
            .grep("println", "test.rs", &GrepOptions::default())
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "test.rs", "{results:?}");
        assert_eq!(results[0].line_number, 2);
        assert!(results[0].line.contains("println"));

        let insensitive = f
            .sandbox
            .grep("PRINTLN", ".", &{
                let mut options = GrepOptions::default();
                options.case_insensitive = true;
                options
            })
            .await
            .unwrap();
        assert_eq!(insensitive.len(), 1);
    }

    #[tokio::test]
    async fn walk_and_glob_report_paths_relative_to_the_declared_base() {
        let f = fixture().await;
        fs::create_dir_all(f.dir.path().join(".ai/reports"))
            .await
            .unwrap();
        fs::create_dir_all(f.dir.path().join(".ai/target"))
            .await
            .unwrap();
        fs::write(f.dir.path().join(".ai/reports/result.md"), "report")
            .await
            .unwrap();
        fs::write(f.dir.path().join(".ai/reports/empty.md"), "")
            .await
            .unwrap();
        fs::write(f.dir.path().join(".ai/target/ignored.md"), "ignored")
            .await
            .unwrap();

        let files = f
            .sandbox
            .walk_files(f.sandbox.working_directory(), ".ai", &{
                let mut options = WalkOptions::default();
                options.exclude_dirs = vec!["target".to_string()];
                options
            })
            .await
            .unwrap();
        let mut metadata: Vec<_> = files
            .iter()
            .map(|file| (file.relative_path.as_str(), file.size))
            .collect();
        metadata.sort_unstable();
        assert_eq!(metadata, vec![
            (".ai/reports/empty.md", 0),
            (".ai/reports/result.md", 6),
        ]);
        let root = f.sandbox.working_directory().to_string();
        assert!(files.iter().all(|file| file.path.starts_with(&root)));

        let globbed = f.sandbox.glob("**/*.md", None).await.unwrap();
        assert_eq!(globbed, vec![
            format!("{root}/.ai/reports/empty.md"),
            format!("{root}/.ai/reports/result.md"),
            format!("{root}/.ai/target/ignored.md"),
        ]);
        let scoped = f.sandbox.glob("*.md", Some(".ai/reports")).await.unwrap();
        assert_eq!(scoped.len(), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn glob_does_not_follow_symlinked_directories_below_the_root() {
        let f = fixture().await;
        let target = f.dir.path().join("elsewhere");
        fs::create_dir_all(&target).await.unwrap();
        fs::write(target.join("lib.rs"), "").await.unwrap();
        std::os::unix::fs::symlink(&target, f.dir.path().join("linked")).unwrap();

        let results = f.sandbox.glob("linked/**/*.rs", None).await.unwrap();
        assert!(results.is_empty(), "{results:?}");
    }

    #[tokio::test]
    async fn download_and_upload_copy_binary_files() {
        let f = fixture().await;
        let bytes = vec![0u8, 159, 146, 150, 255];
        fs::write(f.dir.path().join("source.bin"), &bytes)
            .await
            .unwrap();
        let dest = f.dir.path().join("out/nested/copy.bin");
        f.sandbox
            .download_file_to_local("source.bin", &dest)
            .await
            .unwrap();
        assert_eq!(fs::read(&dest).await.unwrap(), bytes);
        f.sandbox
            .upload_file_from_local(&dest, "in/again.bin")
            .await
            .unwrap();
        assert_eq!(
            f.sandbox.read_file_bytes("in/again.bin").await.unwrap(),
            bytes
        );
    }

    /// Collects the driver's events for assertions.
    struct Recorded(Mutex<Vec<sandbox_driver::Event>>);

    #[async_trait]
    impl sandbox_driver::EventObserver for Recorded {
        async fn observe(&self, event: sandbox_driver::Event) {
            self.0.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn lifecycle_reaches_the_driver_events_and_learns_the_platform() {
        let dir = tempfile::tempdir().unwrap();
        let recorded = Arc::new(Recorded(Mutex::new(Vec::new())));
        let sandbox = RunSandboxSpec::local(dir.path(), ProviderAccess::default())
            .build(Some(EventContext::new(
                Arc::clone(&recorded) as Arc<dyn sandbox_driver::EventObserver>
            )))
            .await
            .unwrap();
        sandbox.initialize().await.unwrap();
        let expected = if cfg!(target_os = "macos") {
            "darwin"
        } else {
            std::env::consts::OS
        };
        assert_eq!(sandbox.platform(), expected);
        assert!(sandbox.os_version().starts_with(expected));
        let handle = Arc::clone(sandbox.handle().unwrap());
        assert_eq!(sandbox.sandbox_info(), handle.id().to_string());
        assert!(
            sandbox.sandbox_info().starts_with("host-dir-"),
            "a local sandbox is identified by its directory: {}",
            sandbox.sandbox_info()
        );
        let isolated = RunSandbox::new(SandboxProviderKind::DOCKER, Arc::clone(&handle));
        assert_eq!(isolated.sandbox_info(), handle.id().to_string());
        assert_eq!(sandbox.console_url().await, None);

        sandbox.stop().await.unwrap();
        sandbox.activate().await.unwrap();
        sandbox.delete().await.unwrap();
        assert!(
            dir.path().is_dir(),
            "designated directories survive cleanup"
        );

        let captured = recorded.0.lock().unwrap();
        let steps: Vec<String> = captured
            .iter()
            .filter_map(|event| match &event.body {
                sandbox_driver::EventBody::OperationStarted { action } => {
                    Some(format!("{action:?} started"))
                }
                sandbox_driver::EventBody::OperationCompleted { action, .. } => {
                    Some(format!("{action:?} completed"))
                }
                sandbox_driver::EventBody::OperationFailed { action, .. } => {
                    Some(format!("{action:?} failed"))
                }
                _ => None,
            })
            .collect();
        assert_eq!(steps, vec![
            "Create started",
            "Create completed",
            "Stop started",
            "Stop completed",
            "Start started",
            "Start completed",
            "Delete started",
            "Delete completed",
        ]);
        assert!(
            captured
                .iter()
                .all(|event| event.provider.to_string() == "host"),
            "the driver names its own provider"
        );
    }

    #[tokio::test]
    async fn local_sandbox_designates_the_directory_and_knows_its_platform() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("fresh");
        let sandbox = local_sandbox(&workspace).await.unwrap();
        assert!(workspace.is_dir(), "a missing working directory is created");
        assert_eq!(sandbox.kind(), &SandboxProviderKind::LOCAL);
        assert_ne!(sandbox.platform(), "unknown");
        assert_eq!(
            Path::new(sandbox.working_directory()),
            workspace.canonicalize().unwrap()
        );
        sandbox.delete().await.unwrap();
        assert!(workspace.is_dir());
    }

    #[tokio::test]
    async fn preview_urls_come_from_the_access_facet() {
        let f = fixture().await;
        let (url, headers) = f.sandbox.get_preview_url(8080).await.unwrap().unwrap();
        assert_eq!(url, "http://127.0.0.1:8080");
        assert!(headers.is_empty());
    }

    #[tokio::test]
    async fn port_routes_answer_pebble_with_the_access_facets_preview_url() {
        let Fixture {
            dir,
            _provider: provider,
            sandbox,
        } = fixture().await;
        let sandbox = Arc::new(sandbox);
        let routes = sandbox
            .port_routes()
            .expect("the host provider routes to its ports");
        let route = routes.route(8080).await.unwrap();
        assert_eq!(route, PortRoute::new("http://127.0.0.1:8080"));
        routes.release(8080).await.unwrap();
        drop((dir, provider));
    }
}
