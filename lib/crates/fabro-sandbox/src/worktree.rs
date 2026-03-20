use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::{shell_quote, DirEntry, ExecResult, GrepOptions, Sandbox};

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
    Reset { sha: String },
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
}

impl WorktreeSandbox {
    /// Create a new `WorktreeSandbox` wrapping `inner` with the given configuration.
    pub fn new(inner: Arc<dyn Sandbox>, config: WorktreeConfig) -> Self {
        Self {
            inner,
            config,
            event_callback: None,
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
}

// ---------------------------------------------------------------------------
// Sandbox implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Sandbox for WorktreeSandbox {
    // --- Lifecycle ---

    /// Set up the git worktree:
    /// 1. Unless `skip_branch_creation`: force-create the branch at `base_sha`, emit `BranchCreated`.
    /// 2. Best-effort remove any stale worktree, then add fresh one, emit `WorktreeAdded`.
    /// 3. Unless `skip_branch_creation`: hard-reset the worktree to `base_sha`, emit `Reset`.
    ///
    /// Does NOT call `inner.initialize()`.
    async fn initialize(&self) -> Result<(), String> {
        let path = shell_quote(&self.config.worktree_path);
        let branch = shell_quote(&self.config.branch_name);
        let sha = shell_quote(&self.config.base_sha);

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

        // Best-effort remove any stale worktree registration + directory
        let rm_cmd = format!("{GIT} worktree remove --force {path}");
        let _ = self
            .inner
            .exec_command(&rm_cmd, 30_000, None, None, None)
            .await;

        let add_cmd = format!("{GIT} worktree add {path} {branch}");
        let result = self
            .inner
            .exec_command(&add_cmd, 30_000, None, None, None)
            .await?;
        if result.exit_code != 0 {
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

        if !self.config.skip_branch_creation {
            let reset_cmd = format!("{GIT} reset --hard {sha}");
            let result = self
                .inner
                .exec_command(
                    &reset_cmd,
                    30_000,
                    Some(&self.config.worktree_path),
                    None,
                    None,
                )
                .await?;
            if result.exit_code != 0 {
                return Err(format!(
                    "git reset --hard failed (exit {}): {}",
                    result.exit_code,
                    result.stderr.trim()
                ));
            }
            self.emit(WorktreeEvent::Reset {
                sha: self.config.base_sha.clone(),
            });
        }

        Ok(())
    }

    /// Remove the git worktree and emit `WorktreeRemoved`. Does NOT call `inner.cleanup()`.
    async fn cleanup(&self) -> Result<(), String> {
        let path = shell_quote(&self.config.worktree_path);
        let cmd = format!("{GIT} worktree remove --force {path}");
        let _ = self
            .inner
            .exec_command(&cmd, 30_000, None, None, None)
            .await;
        self.emit(WorktreeEvent::WorktreeRemoved {
            path: self.config.worktree_path.clone(),
        });
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
        self.inner.read_file(path, offset, limit).await
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        self.inner.write_file(path, content).await
    }

    async fn delete_file(&self, path: &str) -> Result<(), String> {
        self.inner.delete_file(path).await
    }

    async fn file_exists(&self, path: &str) -> Result<bool, String> {
        self.inner.file_exists(path).await
    }

    async fn list_directory(
        &self,
        path: &str,
        depth: Option<usize>,
    ) -> Result<Vec<DirEntry>, String> {
        self.inner.list_directory(path, depth).await
    }

    async fn grep(
        &self,
        pattern: &str,
        path: &str,
        options: &GrepOptions,
    ) -> Result<Vec<String>, String> {
        self.inner.grep(pattern, path, options).await
    }

    async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<Vec<String>, String> {
        self.inner.glob(pattern, path).await
    }

    async fn download_file_to_local(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<(), String> {
        self.inner
            .download_file_to_local(remote_path, local_path)
            .await
    }

    async fn upload_file_from_local(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<(), String> {
        self.inner
            .upload_file_from_local(local_path, remote_path)
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

    fn is_remote(&self) -> bool {
        self.inner.is_remote()
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
        self.inner.mark_agent_read(path);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
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
        // branch --force, worktree remove (best-effort), worktree add, reset --hard
        assert_eq!(cmds.len(), 4, "expected 4 git commands, got: {cmds:?}");
        assert!(cmds[0].contains("branch --force"), "cmd[0]: {}", cmds[0]);
        assert!(
            cmds[1].contains("worktree remove --force"),
            "cmd[1]: {}",
            cmds[1]
        );
        assert!(cmds[2].contains("worktree add"), "cmd[2]: {}", cmds[2]);
        assert!(cmds[3].contains("reset --hard"), "cmd[3]: {}", cmds[3]);
    }

    #[tokio::test]
    async fn initialize_emits_branch_worktree_reset_events() {
        let (inner, _mock) = make_mock();
        let mut wt = WorktreeSandbox::new(inner, make_config("/tmp/wt"));

        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        wt.set_event_callback(Arc::new(move |event| {
            let label = match &event {
                WorktreeEvent::BranchCreated { .. } => "BranchCreated",
                WorktreeEvent::WorktreeAdded { .. } => "WorktreeAdded",
                WorktreeEvent::WorktreeRemoved { .. } => "WorktreeRemoved",
                WorktreeEvent::Reset { .. } => "Reset",
            };
            events_clone.lock().unwrap().push(label.to_string());
        }));

        wt.initialize().await.unwrap();

        let captured = events.lock().unwrap();
        assert_eq!(*captured, vec!["BranchCreated", "WorktreeAdded", "Reset"]);
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
        // The path "/tmp/my worktree" should be quoted in shell commands
        assert!(
            cmds[1].contains("'/tmp/my worktree'") || cmds[1].contains("\"/tmp/my worktree\""),
            "worktree path should be shell-quoted: {}",
            cmds[1]
        );
    }

    #[tokio::test]
    async fn initialize_reset_uses_worktree_path_as_working_dir() {
        let (inner, mock) = make_mock();
        let wt = WorktreeSandbox::new(inner, make_config("/tmp/wt"));

        wt.initialize().await.unwrap();

        let wdirs = mock.captured_working_dirs.lock().unwrap().clone();
        // reset command is at index 3, should use worktree path
        assert_eq!(
            wdirs[3],
            Some("/tmp/wt".to_string()),
            "reset --hard should run in worktree dir"
        );
        // branch, remove, add commands use None (inner's default)
        assert_eq!(wdirs[0], None, "branch command should use inner default");
        assert_eq!(wdirs[1], None, "worktree remove should use inner default");
        assert_eq!(wdirs[2], None, "worktree add should use inner default");
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
        // Only worktree remove (best-effort) and worktree add
        assert_eq!(cmds.len(), 2, "expected 2 git commands, got: {cmds:?}");
        assert!(
            cmds[0].contains("worktree remove --force"),
            "cmd[0]: {}",
            cmds[0]
        );
        assert!(cmds[1].contains("worktree add"), "cmd[1]: {}", cmds[1]);
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
                WorktreeEvent::Reset { .. } => "Reset",
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

    #[tokio::test]
    async fn cleanup_issues_worktree_remove_command() {
        let (inner, mock) = make_mock();
        let wt = WorktreeSandbox::new(inner, make_config("/tmp/wt"));

        wt.cleanup().await.unwrap();

        let cmds = mock.captured_commands.lock().unwrap().clone();
        assert_eq!(cmds.len(), 1, "cleanup should issue exactly one command");
        assert!(
            cmds[0].contains("worktree remove --force"),
            "cleanup command should remove the worktree: {}",
            cmds[0]
        );
    }

    #[tokio::test]
    async fn cleanup_emits_worktree_removed_event() {
        let (inner, _mock) = make_mock();
        let mut wt = WorktreeSandbox::new(inner, make_config("/tmp/wt"));

        let removed_path: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let path_clone = Arc::clone(&removed_path);
        wt.set_event_callback(Arc::new(move |event| {
            if let WorktreeEvent::WorktreeRemoved { path } = event {
                *path_clone.lock().unwrap() = Some(path);
            }
        }));

        wt.cleanup().await.unwrap();

        assert_eq!(*removed_path.lock().unwrap(), Some("/tmp/wt".to_string()));
    }

    #[tokio::test]
    async fn cleanup_succeeds_even_if_worktree_remove_fails() {
        let inner: Arc<dyn Sandbox> = Arc::new(MockSandbox {
            exec_result: ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 1, // non-zero, but cleanup should still succeed
                timed_out: false,
                duration_ms: 0,
            },
            ..MockSandbox::linux()
        });
        let wt = WorktreeSandbox::new(inner, make_config("/tmp/wt"));

        let result = wt.cleanup().await;
        assert!(result.is_ok(), "cleanup should succeed even if git fails");
    }

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
