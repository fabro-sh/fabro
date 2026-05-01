use fabro_agent::Sandbox;
use fabro_sandbox::shell_quote;
use tokio::sync::OnceCell;

use crate::sandbox_git::GIT_REMOTE;

pub(crate) struct SandboxGitRuntime {
    probe: OnceCell<Result<(), String>>,
}

impl SandboxGitRuntime {
    pub(crate) fn new() -> Self {
        Self {
            probe: OnceCell::new(),
        }
    }

    pub(crate) async fn ensure_git_available(&self, sandbox: &dyn Sandbox) -> Result<(), String> {
        self.probe
            .get_or_init(|| async { probe_sandbox_git(sandbox).await })
            .await
            .clone()
    }
}

impl Default for SandboxGitRuntime {
    fn default() -> Self {
        Self::new()
    }
}

async fn probe_sandbox_git(sandbox: &dyn Sandbox) -> Result<(), String> {
    let temp = sandbox_temp_dir(sandbox, "probe", "git");
    let index = format!("{temp}/index");
    let probe_file = format!("{temp}/probe.txt");
    let command = format!(
        "set -e\n\
         rm -rf {temp_q}\n\
         mkdir -p {temp_q}\n\
         printf probe > {probe_file_q}\n\
         GIT_INDEX_FILE={index_q} {git} read-tree --empty\n\
         blob=$({git} hash-object -w {probe_file_q})\n\
         GIT_INDEX_FILE={index_q} {git} update-index --add --cacheinfo 100644,$blob,probe.txt\n\
         GIT_INDEX_FILE={index_q} {git} write-tree >/dev/null\n\
         rm -rf {temp_q}",
        temp_q = shell_quote(&temp),
        probe_file_q = shell_quote(&probe_file),
        index_q = shell_quote(&index),
        git = GIT_REMOTE,
    );
    exec_ok(sandbox, &command).await
}

fn sandbox_temp_dir(sandbox: &dyn Sandbox, run_id: &str, label: &str) -> String {
    let cwd = sandbox.working_directory().trim_end_matches('/');
    let id = uuid::Uuid::new_v4();
    format!("{cwd}/.fabro/tmp/{label}-{run_id}-{id}")
}

async fn exec_ok(sandbox: &dyn Sandbox, command: &str) -> Result<(), String> {
    let result = sandbox
        .exec_command(command, 30_000, None, None, None)
        .await
        .map_err(|err| err.display_with_causes())?;
    if result.is_success() {
        Ok(())
    } else {
        Err(exec_err(command, &result))
    }
}

fn exec_err(label: &str, result: &fabro_sandbox::ExecResult) -> String {
    if result.is_timed_out() {
        return format!("{label} timed out after {}ms", result.duration_ms);
    }
    if result.is_cancelled() {
        return format!("{label} cancelled after {}ms", result.duration_ms);
    }
    let detail = format!("{}{}", result.stdout, result.stderr);
    let detail = detail.trim();
    if detail.is_empty() {
        format!("{label} failed with exit {}", result.display_exit_code())
    } else {
        format!(
            "{label} failed with exit {}: {detail}",
            result.display_exit_code()
        )
    }
}
