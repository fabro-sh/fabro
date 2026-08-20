use fabro_agent::Sandbox;
use fabro_sandbox::shell_quote;
use fabro_util::error::SharedError;
use tokio::sync::OnceCell;

use crate::sandbox_git::{GIT_REMOTE, exec_err};

pub(crate) struct SandboxGitRuntime {
    probe: OnceCell<Result<(), SharedError>>,
}

impl SandboxGitRuntime {
    pub(crate) fn new() -> Self {
        Self {
            probe: OnceCell::new(),
        }
    }

    pub(crate) async fn ensure_git_available(
        &self,
        sandbox: &dyn Sandbox,
    ) -> Result<(), SharedError> {
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

async fn probe_sandbox_git(sandbox: &dyn Sandbox) -> Result<(), SharedError> {
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

async fn exec_ok(sandbox: &dyn Sandbox, command: &str) -> Result<(), SharedError> {
    let result = sandbox
        .exec_command(command, 30_000, None, None, None)
        .await
        .map_err(|err| {
            SharedError::new(anyhow::Error::new(err).context("sandbox git probe command failed"))
        })?;
    if result.is_success() {
        Ok(())
    } else {
        Err(SharedError::new(anyhow::Error::new(exec_err(
            command, result,
        ))))
    }
}
