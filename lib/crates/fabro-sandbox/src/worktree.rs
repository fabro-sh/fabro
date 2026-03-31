use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::{DirEntry, ExecResult, GrepOptions, Sandbox, shell_quote};

/// Git command prefix that disables background maintenance.
const GIT: &str = "git -c maintenance.auto=0 -c gc.auto=0";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Events emitted during worktree lifecycle operations.
pub enum WorktreeEvent {
    BranchCreated { branch: String, sha: String },
    WorktreeAdded { path: String, branch: String },
    WorktreeRemoved { path: String },
}

/// Callback type for worktree lifecycle events.
pub type WorktreeEventCallback = Arc<dyn Fn(WorktreeEvent) + Send + Sync>;

/// Configuration for a `WorktreeSandbox`.
pub struct WorktreeConfig {
    pub branch_name: String,
    pub base_sha: String,
    pub worktree_path: String,
    /// Skip branch creation and hard reset (for resume, where branch already exists).
    pub skip_branch_creation: bool,
}

/// Wraps any `Sandbox`, manages a git worktree lifecycle in `initialize()`/`cleanup()`,
/// and overrides `working_directory()` and `exec_command()` to use the worktree path.
///
/// `initialize()` and `cleanup()` do NOT call the inner sandbox's lifecycle methods.
/// The inner sandbox's lifecycle is managed separately by the caller.
pub struct WorktreeSandbox {
    inner: Arc<dyn Sandbox>,
    config: WorktreeConfig,
    event_callback: Option<WorktreeEventCallback>,
    initialized: std::sync::atomic::AtomicBool,
}

impl WorktreeSandbox {
    /// Create a new `WorktreeSandbox` wrapping `inner` with the given configuration.
    pub fn new(inner: Arc<dyn Sandbox>, config: WorktreeConfig) -> Self {
        Self {
            inner,
            config,
            event_callback: None,
            initialized: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Set the callback to receive worktree lifecycle events.
    pub fn set_event_callback(&mut self, cb: WorktreeEventCallback) {
        self.event_callback = Some(cb);
    }

    /// The git branch name managed by this sandbox.
    pub fn branch_name(&self) -> &str {
        &self.config.branch_name
    }

    /// The base commit SHA used when initializing the worktree.
    pub fn base_sha(&self) -> &str {
        &self.config.base_sha
    }

    /// The filesystem path to the worktree directory.
    pub fn worktree_path(&self) -> &str {
        &self.config.worktree_path
    }

    fn emit(&self, event: WorktreeEvent) {
        if let Some(ref cb) = self.event_callback {
            cb(event);
        }
    }

    fn resolve_path(&self, path: &str) -> String {
        if std::path::Path::new(path).is_absolute() {
            path.to_string()
        } else {
            format!("{}/{path}", self.config.worktree_path)
        }
    }
}

// ---------------------------------------------------------------------------
// Sandbox implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Sandbox for WorktreeSandbox {
    // --- Lifecycle ---

    /// Set up the git worktree:
    /// 1. Best-effort remove any stale worktree at `path` (so the branch is free to be updated).
    /// 2. Unless `skip_branch_creation`: force-create the branch at `base_sha`, emit `BranchCreated`.
    /// 3. Add the worktree, emit `WorktreeAdded`.
    ///
    /// Does NOT call `inner.initialize()`.
    async fn initialize(&self) -> Result<(), String> {
        if self
            .initialized
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(());
        }
        let path = shell_quote(&self.config.worktree_path);
        let branch = shell_quote(&self.config.branch_name);
        let sha = shell_quote(&self.config.base_sha);

        // Best-effort remove any stale worktree registration + directory first,
        // so that the branch is not "in use" when we try to force-update it.
        let rm_cmd = format!("{GIT} worktree remove --force {path}");
        let _ = self
            .inner
            .exec_command(&rm_cmd, 30_000, None, None, None)
            .await;

        // Prune all stale worktree references whose directories no longer exist.
        // Without this, a branch may remain locked by a worktree in a deleted
        // temp directory from a previous run.
        let prune_cmd = format!("{GIT} worktree prune");
        let _ = self
            .inner
            .exec_command(&prune_cmd, 30_000, None, None, None)
            .await;

        if !self.config.skip_branch_creation {
            let cmd = format!("{GIT} branch --force {branch} {sha}");
            let result = self
                .inner
                .exec_command(&cmd, 30_000, None, None, None)
                .await?;
            if result.exit_code != 0 {
                return Err(format!(
                    "git branch --force failed (exit {}): {}",
                    result.exit_code,
                    result.stderr.trim()
                ));
            }
            self.emit(WorktreeEvent::BranchCreated {
                branch: self.config.branch_name.clone(),
                sha: self.config.base_sha.clone(),
            });
        }

        let add_cmd = format!("{GIT} worktree add {path} {branch}");
        let result = self
            .inner
            .exec_command(&add_cmd, 30_000, None, None, None)
            .await?;
        if result.exit_code != 0 {
            // Roll back the branch created above so we don't leak partial state.
            if !self.config.skip_branch_creation {
                let rollback_cmd = format!("{GIT} branch -D {branch}");
                let _ = self
                    .inner
                    .exec_command(&rollback_cmd, 30_000, None, None, None)
                    .await;
            }
            return Err(format!(
                "git worktree add failed (exit {}): {}",
                result.exit_code,
                result.stderr.trim()
            ));
        }
        self.emit(WorktreeEvent::WorktreeAdded {
            path: self.config.worktree_path.clone(),
            branch: self.config.branch_name.clone(),
        });

        Ok(())
    }

    /// No-op — the worktree must survive cleanup for `fabro cp` access.
    /// Worktrees are pruned separately by `system prune`.
    async fn cleanup(&self) -> Result<(), String> {
        Ok(())
    }

    fn working_directory(&self) -> &str {
        &self.config.worktree_path
    }

    /// Execute a command, defaulting `working_dir` to the worktree path when `None`.
    async fn exec_command(
        &self,
        command: &str,
        timeout_ms: u64,
        working_dir: Option<&str>,
        env_vars: Option<&HashMap<String, String>>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<ExecResult, String> {
        let wd = working_dir.unwrap_or(&self.config.worktree_path);
        self.inner
            .exec_command(command, timeout_ms, Some(wd), env_vars, cancel_token)
            .await
    }

    // --- Delegated methods ---

    async fn read_file(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, String> {
        let resolved = self.resolve_path(path);
        self.inner.read_file(&resolved, offset, limit).await
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        let resolved = self.resolve_path(path);
        self.inner.write_file(&resolved, content).await
    }

    async fn delete_file(&self, path: &str) -> Result<(), String> {
        let resolved = self.resolve_path(path);
        self.inner.delete_file(&resolved).await
    }

    async fn file_exists(&self, path: &str) -> Result<bool, String> {
        let resolved = self.resolve_path(path);
        self.inner.file_exists(&resolved).await
    }

    async fn list_directory(
        &self,
        path: &str,
        depth: Option<usize>,
    ) -> Result<Vec<DirEntry>, String> {
        let resolved = self.resolve_path(path);
        self.inner.list_directory(&resolved, depth).await
    }

    async fn grep(
        &self,
        pattern: &str,
        path: &str,
        options: &GrepOptions,
    ) -> Result<Vec<String>, String> {
        let resolved = self.resolve_path(path);
        self.inner.grep(pattern, &resolved, options).await
    }

    async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<Vec<String>, String> {
        let resolved = path.map(|p| self.resolve_path(p));
        let glob_path = resolved.as_deref().unwrap_or(&self.config.worktree_path);
        self.inner.glob(pattern, Some(glob_path)).await
    }

    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<(), String> {
        let resolved = self.resolve_path(remote_path);
        self.inner
            .download_file_to_local(&resolved, local_path)
            .await
    }

    async fn upload_file_from_local(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<(), String> {
        let resolved = self.resolve_path(remote_path);
        self.inner
            .upload_file_from_local(local_path, &resolved)
            .await
    }

    fn platform(&self) -> &str {
        self.inner.platform()
    }

    fn os_version(&self) -> String {
        self.inner.os_version()
    }

    fn sandbox_info(&self) -> String {
        self.inner.sandbox_info()
    }

    async fn refresh_push_credentials(&self) -> Result<(), String> {
        self.inner.refresh_push_credentials().await
    }

    async fn set_autostop_interval(&self, minutes: i32) -> Result<(), String> {
        self.inner.set_autostop_interval(minutes).await
    }

    fn host_git_dir(&self) -> Option<&str> {
        Some(&self.config.worktree_path)
    }

    async fn setup_git_for_run(&self, run_id: &str) -> Result<Option<crate::GitRunInfo>, String> {
        self.inner.setup_git_for_run(run_id).await
    }

    fn resume_setup_commands(&self, run_branch: &str) -> Vec<String> {
        self.inner.resume_setup_commands(run_branch)
    }

    async fn git_push_branch(&self, branch: &str) -> bool {
        self.inner.git_push_branch(branch).await
    }

    fn parallel_worktree_path(
        &self,
        run_dir: &Path,
        run_id: &str,
        node_id: &str,
        key: &str,
    ) -> String {
        self.inner
            .parallel_worktree_path(run_dir, run_id, node_id, key)
    }

    async fn ssh_access_command(&self) -> Result<Option<String>, String> {
        self.inner.ssh_access_command().await
    }

    fn origin_url(&self) -> Option<&str> {
        self.inner.origin_url()
    }

    async fn get_preview_url(
        &self,
        port: u16,
    ) -> Result<Option<(String, HashMap<String, String>)>, String> {
        self.inner.get_preview_url(port).await
    }

    fn mark_agent_read(&self, path: &str) {
        let resolved = self.resolve_path(path);
        self.inner.mark_agent_read(&resolved);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalSandbox;
    use crate::test_support::MockSandbox;
    use std::sync::Mutex;

    fn make_config(wt_path: &str) -> WorktreeConfig {
        WorktreeConfig {
            branch_name: "fabro/run/test-branch".to_string(),
            base_sha: "abc123def456".to_string(),
            worktree_path: wt_path.to_string(),
            skip_branch_creation: false,
        }
    }

    fn make_config_skip(wt_path: &str) -> WorktreeConfig {
        WorktreeConfig {
            branch_name: "fabro/run/test-branch".to_string(),
            base_sha: "abc123def456".to_string(),
            worktree_path: wt_path.to_string(),
            skip_branch_creation: true,
        }
    }

    /// Create a shared mock and return both the `Arc<dyn Sandbox>` (passed to WorktreeSandbox)
    /// and the `Arc<MockSandbox>` (used to assert captured state).
    fn make_mock() -> (Arc<dyn Sandbox>, Arc<MockSandbox>) {
        let mock = Arc::new(MockSandbox::linux());
        let as_sandbox: Arc<dyn Sandbox> = mock.clone();
        (as_sandbox, mock)
    }

    // -----------------------------------------------------------------------
    // initialize() — full setup (skip_branch_creation = false)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn initialize_issues_correct_git_commands() {
        let (inner, mock) = make_mock();
        let wt = WorktreeSandbox::new(inner, make_config("/tmp/wt"));

        wt.initialize().await.unwrap();

        let cmds = mock.captured_commands.lock().unwrap().clone();
        // worktree remove (best-effort), worktree prune, branch --force, worktree add
        assert_eq!(cmds.len(), 4, "expected 4 git commands, got: {cmds:?}");
        assert!(
            cmds[0].contains("worktree remove --force"),
            "cmd[0]: {}",
            cmds[0]
        );
        assert!(cmds[1].contains("worktree prune"), "cmd[1]: {}", cmds[1]);
        assert!(cmds[2].contains("branch --force"), "cmd[2]: {}", cmds[2]);
        assert!(cmds[3].contains("worktree add"), "cmd[3]: {}", cmds[3]);
    }

    #[tokio::test]
    async fn initialize_emits_branch_and_worktree_events() {
        let (inner, _mock) = make_mock();
        let mut wt = WorktreeSandbox::new(inner, make_config("/tmp/wt"));

        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        wt.set_event_callback(Arc::new(move |event| {
            let label = match &event {
                WorktreeEvent::BranchCreated { .. } => "BranchCreated",
                WorktreeEvent::WorktreeAdded { .. } => "WorktreeAdded",
                WorktreeEvent::WorktreeRemoved { .. } => "WorktreeRemoved",
            };
            events_clone.lock().unwrap().push(label.to_string());
        }));

        wt.initialize().await.unwrap();

        let captured = events.lock().unwrap();
        assert_eq!(*captured, vec!["BranchCreated", "WorktreeAdded"]);
    }

    #[tokio::test]
    async fn initialize_uses_shell_quoted_values_in_commands() {
        let (inner, mock) = make_mock();
        let config = WorktreeConfig {
            branch_name: "fabro/run/my-branch".to_string(),
            base_sha: "deadbeef".to_string(),
            worktree_path: "/tmp/my worktree".to_string(), // path with space
            skip_branch_creation: false,
        };
        let wt = WorktreeSandbox::new(inner, config);

        wt.initialize().await.unwrap();

        let cmds = mock.captured_commands.lock().unwrap().clone();
        // The path "/tmp/my worktree" should be quoted in the worktree remove command (cmd[0])
        assert!(
            cmds[0].contains("'/tmp/my worktree'") || cmds[0].contains("\"/tmp/my worktree\""),
            "worktree path should be shell-quoted: {}",
            cmds[0]
        );
    }

    // -----------------------------------------------------------------------
    // initialize() — skip_branch_creation = true
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn initialize_skip_branch_creation_issues_only_worktree_commands() {
        let (inner, mock) = make_mock();
        let wt = WorktreeSandbox::new(inner, make_config_skip("/tmp/wt"));

        wt.initialize().await.unwrap();

        let cmds = mock.captured_commands.lock().unwrap().clone();
        // worktree remove (best-effort), worktree prune, worktree add
        assert_eq!(cmds.len(), 3, "expected 3 git commands, got: {cmds:?}");
        assert!(
            cmds[0].contains("worktree remove --force"),
            "cmd[0]: {}",
            cmds[0]
        );
        assert!(cmds[1].contains("worktree prune"), "cmd[1]: {}", cmds[1]);
        assert!(cmds[2].contains("worktree add"), "cmd[2]: {}", cmds[2]);
    }

    #[tokio::test]
    async fn initialize_skip_branch_creation_emits_only_worktree_added() {
        let (inner, _mock) = make_mock();
        let mut wt = WorktreeSandbox::new(inner, make_config_skip("/tmp/wt"));

        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        wt.set_event_callback(Arc::new(move |event| {
            let label = match &event {
                WorktreeEvent::BranchCreated { .. } => "BranchCreated",
                WorktreeEvent::WorktreeAdded { .. } => "WorktreeAdded",
                WorktreeEvent::WorktreeRemoved { .. } => "WorktreeRemoved",
            };
            events_clone.lock().unwrap().push(label.to_string());
        }));

        wt.initialize().await.unwrap();

        let captured = events.lock().unwrap();
        assert_eq!(*captured, vec!["WorktreeAdded"]);
    }

    // -----------------------------------------------------------------------
    // initialize() — error propagation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn initialize_propagates_error_on_nonzero_exit() {
        let inner: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout: String::new(),
                stderr: "fatal: not a git repo".to_string(),
                exit_code: 128,
                timed_out: false,
                duration_ms: 5,
            },
            ..MockSandbox::linux()
        });
        let wt = WorktreeSandbox::new(inner, make_config("/tmp/wt"));

        let result = wt.initialize().await;

        assert!(result.is_err(), "should return Err on non-zero exit");
        let err = result.unwrap_err();
        assert!(
            err.contains("branch --force failed") || err.contains("128"),
            "error should mention the failure: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // cleanup()
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // working_directory()
    // -----------------------------------------------------------------------

    #[test]
    fn working_directory_returns_worktree_path() {
        let (inner, _mock) = make_mock();
        let wt = WorktreeSandbox::new(inner, make_config("/tmp/my_worktree"));

        assert_eq!(wt.working_directory(), "/tmp/my_worktree");
    }

    // -----------------------------------------------------------------------
    // exec_command() working_dir defaulting
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn exec_command_none_working_dir_defaults_to_worktree_path() {
        let (inner, mock) = make_mock();
        let wt = WorktreeSandbox::new(inner, make_config("/tmp/wt"));

        wt.exec_command("echo hello", 5000, None, None, None)
            .await
            .unwrap();

        let wdirs = mock.captured_working_dirs.lock().unwrap().clone();
        assert_eq!(
            wdirs.last(),
            Some(&Some("/tmp/wt".to_string())),
            "None working_dir should be replaced with worktree path"
        );
    }

    #[tokio::test]
    async fn exec_command_explicit_working_dir_passes_through() {
        let (inner, mock) = make_mock();
        let wt = WorktreeSandbox::new(inner, make_config("/tmp/wt"));

        wt.exec_command("echo hello", 5000, Some("/explicit/path"), None, None)
            .await
            .unwrap();

        let wdirs = mock.captured_working_dirs.lock().unwrap().clone();
        assert_eq!(
            wdirs.last(),
            Some(&Some("/explicit/path".to_string())),
            "explicit working_dir should be passed through unchanged"
        );
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Bug: cleanup() destroys worktree, breaking `fabro cp`
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn cleanup_should_preserve_worktree_for_post_run_access() {
        // The worktree directory must survive cleanup() so that `fabro cp` can
        // access run artifacts afterward. It is pruned separately by `system prune`.
        // LocalSandbox.cleanup() was a no-op; WorktreeSandbox should match.
        let (inner, mock) = make_mock();
        let wt = WorktreeSandbox::new(inner, make_config("/tmp/wt"));

        wt.cleanup().await.unwrap();

        let cmds = mock.captured_commands.lock().unwrap().clone();
        assert!(
            cmds.is_empty(),
            "cleanup should not issue destructive git commands \
             (worktree must be preserved for fabro cp), but got: {cmds:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Bug: initialize() is not idempotent — double call destroys worktree
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn initialize_is_idempotent_on_second_call() {
        // engine.run_with_lifecycle() calls sandbox.initialize() unconditionally,
        // even when run.rs already called it during sandbox construction.
        // The second call must be a no-op; it must NOT re-run
        // `git worktree remove --force` which would destroy the worktree.
        let (inner, mock) = make_mock();
        let wt = WorktreeSandbox::new(inner, make_config("/tmp/wt"));

        wt.initialize().await.unwrap();
        let first_count = mock.captured_commands.lock().unwrap().len();

        wt.initialize().await.unwrap();
        let second_count = mock.captured_commands.lock().unwrap().len();

        assert_eq!(
            first_count,
            second_count,
            "second initialize() should be a no-op, but it issued {} additional commands",
            second_count - first_count
        );
    }

    // -----------------------------------------------------------------------
    // Bug: file operations resolve against inner working_directory, not worktree
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn grep_should_search_worktree_not_inner_working_directory() {
        // WorktreeSandbox delegates grep() to the inner sandbox without path
        // adjustment. When the inner LocalSandbox was created with original_cwd,
        // grep("pattern", ".") searches the original repo instead of the worktree.
        let original =
            std::env::temp_dir().join(format!("fabro-test-original-{}", uuid::Uuid::new_v4()));
        let worktree =
            std::env::temp_dir().join(format!("fabro-test-worktree-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&original).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();

        // Put a marker file ONLY in the worktree directory
        std::fs::write(worktree.join("marker.txt"), "UNIQUE_WORKTREE_MARKER").unwrap();

        let inner: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(original.clone()));
        let config = WorktreeConfig {
            branch_name: "test-branch".into(),
            base_sha: "abc123".into(),
            worktree_path: worktree.to_string_lossy().to_string(),
            skip_branch_creation: false,
        };
        let wt = WorktreeSandbox::new(inner, config);

        // working_directory() correctly returns the worktree path
        assert_eq!(wt.working_directory(), worktree.to_string_lossy().as_ref());

        // grep with "." should search the worktree, not the original repo
        let results = wt
            .grep("UNIQUE_WORKTREE_MARKER", ".", &GrepOptions::default())
            .await
            .unwrap();
        assert!(
            !results.is_empty(),
            "grep(\".\") should search the worktree directory, not the inner sandbox's working directory"
        );

        std::fs::remove_dir_all(&original).ok();
        std::fs::remove_dir_all(&worktree).ok();
    }

    #[tokio::test]
    async fn glob_should_search_worktree_when_path_is_none() {
        // WorktreeSandbox delegates glob() to the inner sandbox without path
        // adjustment. LocalSandbox::glob(pattern, None) defaults to
        // self.working_directory, which is the original repo path.
        let original =
            std::env::temp_dir().join(format!("fabro-test-original-{}", uuid::Uuid::new_v4()));
        let worktree =
            std::env::temp_dir().join(format!("fabro-test-worktree-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&original).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();

        // Put a file ONLY in the worktree directory
        std::fs::write(worktree.join("worktree_only.txt"), "content").unwrap();

        let inner: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(original.clone()));
        let config = WorktreeConfig {
            branch_name: "test-branch".into(),
            base_sha: "abc123".into(),
            worktree_path: worktree.to_string_lossy().to_string(),
            skip_branch_creation: false,
        };
        let wt = WorktreeSandbox::new(inner, config);

        let results = wt.glob("*.txt", None).await.unwrap();
        assert!(
            results.iter().any(|r| r.contains("worktree_only.txt")),
            "glob(pattern, None) should search the worktree directory, not the inner sandbox's working directory. Got: {results:?}"
        );

        std::fs::remove_dir_all(&original).ok();
        std::fs::remove_dir_all(&worktree).ok();
    }

    #[tokio::test]
    async fn read_file_relative_should_resolve_against_worktree() {
        // WorktreeSandbox delegates read_file() to the inner sandbox without
        // path adjustment. Relative paths resolve against the inner
        // LocalSandbox's working_directory (original repo), not the worktree.
        let original =
            std::env::temp_dir().join(format!("fabro-test-original-{}", uuid::Uuid::new_v4()));
        let worktree =
            std::env::temp_dir().join(format!("fabro-test-worktree-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&original).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();

        // Put the file ONLY in the worktree directory
        std::fs::write(worktree.join("only_in_worktree.txt"), "worktree content").unwrap();

        let inner: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(original.clone()));
        let config = WorktreeConfig {
            branch_name: "test-branch".into(),
            base_sha: "abc123".into(),
            worktree_path: worktree.to_string_lossy().to_string(),
            skip_branch_creation: false,
        };
        let wt = WorktreeSandbox::new(inner, config);

        let result = wt.read_file("only_in_worktree.txt", None, None).await;
        assert!(
            result.is_ok(),
            "read_file with relative path should resolve against worktree, not inner sandbox's working directory. Error: {}",
            result.unwrap_err()
        );

        std::fs::remove_dir_all(&original).ok();
        std::fs::remove_dir_all(&worktree).ok();
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    #[test]
    fn accessors_return_config_values() {
        let (inner, _mock) = make_mock();
        let config = WorktreeConfig {
            branch_name: "my-branch".to_string(),
            base_sha: "sha123".to_string(),
            worktree_path: "/path/to/wt".to_string(),
            skip_branch_creation: false,
        };
        let wt = WorktreeSandbox::new(inner, config);

        assert_eq!(wt.branch_name(), "my-branch");
        assert_eq!(wt.base_sha(), "sha123");
        assert_eq!(wt.worktree_path(), "/path/to/wt");
    }
}
