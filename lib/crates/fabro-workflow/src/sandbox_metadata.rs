use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use fabro_agent::Sandbox;
use fabro_sandbox::shell_quote;
use tokio::fs;
use tokio::sync::OnceCell;

use crate::git::{GitAuthor, META_BRANCH_PREFIX};
use crate::run_dump::RunDump;
use crate::sandbox_git::GIT_REMOTE;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SandboxMetadataError {
    #[error("sandbox git unavailable: {0}")]
    GitUnavailable(String),
    #[error("metadata dump serialization failed: {0}")]
    Dump(#[from] anyhow::Error),
    #[error("metadata temp file write failed: {0}")]
    LocalTemp(std::io::Error),
    #[error("{0}")]
    Git(String),
    #[error("{0}")]
    Sandbox(String),
}

pub(crate) struct SandboxGitRuntime {
    probe:                    OnceCell<Result<(), String>>,
    metadata_degraded:        AtomicBool,
    metadata_warning_emitted: AtomicBool,
}

impl SandboxGitRuntime {
    pub(crate) fn new() -> Self {
        Self {
            probe:                    OnceCell::new(),
            metadata_degraded:        AtomicBool::new(false),
            metadata_warning_emitted: AtomicBool::new(false),
        }
    }

    pub(crate) async fn ensure_git_available(&self, sandbox: &dyn Sandbox) -> Result<(), String> {
        self.probe
            .get_or_init(|| async { probe_sandbox_git(sandbox).await })
            .await
            .clone()
    }

    pub(crate) fn mark_metadata_degraded(&self) -> bool {
        self.metadata_degraded.store(true, Ordering::SeqCst);
        !self.metadata_warning_emitted.swap(true, Ordering::SeqCst)
    }

    pub(crate) fn metadata_degraded(&self) -> bool {
        self.metadata_degraded.load(Ordering::SeqCst)
    }
}

impl Default for SandboxGitRuntime {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn metadata_branch_name(run_id: &str) -> String {
    format!("{META_BRANCH_PREFIX}{run_id}")
}

pub(crate) struct SandboxMetadataWriter<'a> {
    sandbox:    &'a dyn Sandbox,
    runtime:    &'a SandboxGitRuntime,
    run_id:     &'a str,
    branch:     &'a str,
    git_author: GitAuthor,
}

pub(crate) struct MetadataSnapshot {
    pub commit_sha: String,
    pub pushed:     bool,
}

impl<'a> SandboxMetadataWriter<'a> {
    pub(crate) fn new(
        sandbox: &'a dyn Sandbox,
        runtime: &'a SandboxGitRuntime,
        run_id: &'a str,
        branch: &'a str,
        git_author: GitAuthor,
    ) -> Self {
        Self {
            sandbox,
            runtime,
            run_id,
            branch,
            git_author,
        }
    }

    pub(crate) async fn write_snapshot(
        &self,
        dump: &RunDump,
        message: &str,
    ) -> Result<MetadataSnapshot, SandboxMetadataError> {
        self.runtime
            .ensure_git_available(self.sandbox)
            .await
            .map_err(SandboxMetadataError::GitUnavailable)?;

        let entries = dump.git_entries()?;
        let temp = sandbox_temp_dir(self.sandbox, self.run_id, "metadata");
        let index = format!("{temp}/index");
        exec_ok(
            self.sandbox,
            &format!(
                "rm -rf {temp_q} && mkdir -p {temp_q}",
                temp_q = shell_quote(&temp)
            ),
            None,
        )
        .await?;

        let result = self
            .write_snapshot_in_temp(&entries, message, &temp, &index)
            .await;
        let _ = exec_ok(
            self.sandbox,
            &format!("rm -rf {}", shell_quote(&temp)),
            None,
        )
        .await;
        result
    }

    async fn write_snapshot_in_temp(
        &self,
        entries: &[(String, Vec<u8>)],
        message: &str,
        temp: &str,
        index: &str,
    ) -> Result<MetadataSnapshot, SandboxMetadataError> {
        let full_ref = format!("refs/heads/{}", self.branch);
        let env = git_index_env(index, &self.git_author);
        let old_commit = self.load_previous_tree(&full_ref, &env).await?;

        for (ordinal, (path, bytes)) in entries.iter().enumerate() {
            validate_metadata_path(path)?;
            let local = tempfile::NamedTempFile::new().map_err(SandboxMetadataError::LocalTemp)?;
            fs::write(local.path(), bytes)
                .await
                .map_err(SandboxMetadataError::LocalTemp)?;
            let remote = format!("{temp}/blob-{ordinal}");
            self.sandbox
                .upload_file_from_local(local.path(), &remote)
                .await
                .map_err(|err| SandboxMetadataError::Sandbox(err.display_with_causes()))?;
            let hash = exec_stdout(
                self.sandbox,
                &format!("{GIT_REMOTE} hash-object -w {}", shell_quote(&remote)),
                None,
            )
            .await?;
            let cacheinfo = format!("100644,{hash},{path}");
            exec_ok(
                self.sandbox,
                &format!(
                    "{GIT_REMOTE} update-index --add --cacheinfo {}",
                    shell_quote(&cacheinfo)
                ),
                Some(&env),
            )
            .await?;
        }

        let tree = exec_stdout(
            self.sandbox,
            &format!("{GIT_REMOTE} write-tree"),
            Some(&env),
        )
        .await?;
        let message_path = format!("{temp}/message.txt");
        let mut commit_message = message.to_string();
        self.git_author.append_footer(&mut commit_message);
        self.sandbox
            .write_file(&message_path, &commit_message)
            .await
            .map_err(|err| SandboxMetadataError::Sandbox(err.display_with_causes()))?;
        let parent = old_commit
            .as_ref()
            .map_or(String::new(), |sha| format!(" -p {}", shell_quote(sha)));
        let commit = exec_stdout(
            self.sandbox,
            &format!(
                "{GIT_REMOTE} commit-tree {}{parent} -F {}",
                shell_quote(&tree),
                shell_quote(&message_path)
            ),
            Some(&env),
        )
        .await?;
        exec_ok(
            self.sandbox,
            &format!(
                "{GIT_REMOTE} update-ref {} {}",
                shell_quote(&full_ref),
                shell_quote(&commit)
            ),
            None,
        )
        .await?;
        let refspec = format!("{full_ref}:{full_ref}");
        let pushed = self.sandbox.git_push_ref(&refspec).await;
        Ok(MetadataSnapshot {
            commit_sha: commit,
            pushed,
        })
    }

    async fn load_previous_tree(
        &self,
        full_ref: &str,
        env: &HashMap<String, String>,
    ) -> Result<Option<String>, SandboxMetadataError> {
        let old_commit = exec_stdout(
            self.sandbox,
            &format!(
                "{GIT_REMOTE} rev-parse --verify -q {}^{{commit}} || true",
                shell_quote(full_ref)
            ),
            None,
        )
        .await?;
        if old_commit.is_empty() {
            exec_ok(
                self.sandbox,
                &format!("{GIT_REMOTE} read-tree --empty"),
                Some(env),
            )
            .await?;
            Ok(None)
        } else {
            exec_ok(
                self.sandbox,
                &format!("{GIT_REMOTE} read-tree {}", shell_quote(&old_commit)),
                Some(env),
            )
            .await?;
            Ok(Some(old_commit))
        }
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
    exec_ok(sandbox, &command, None)
        .await
        .map_err(|err| err.to_string())
}

fn sandbox_temp_dir(sandbox: &dyn Sandbox, run_id: &str, label: &str) -> String {
    let cwd = sandbox.working_directory().trim_end_matches('/');
    let id = uuid::Uuid::new_v4();
    format!("{cwd}/.fabro/tmp/{label}-{run_id}-{id}")
}

fn git_index_env(index: &str, author: &GitAuthor) -> HashMap<String, String> {
    HashMap::from([
        ("GIT_INDEX_FILE".to_string(), index.to_string()),
        ("GIT_AUTHOR_NAME".to_string(), author.name.clone()),
        ("GIT_AUTHOR_EMAIL".to_string(), author.email.clone()),
        ("GIT_COMMITTER_NAME".to_string(), author.name.clone()),
        ("GIT_COMMITTER_EMAIL".to_string(), author.email.clone()),
    ])
}

async fn exec_stdout(
    sandbox: &dyn Sandbox,
    command: &str,
    env: Option<&HashMap<String, String>>,
) -> Result<String, SandboxMetadataError> {
    let result = sandbox
        .exec_command(command, 30_000, None, env, None)
        .await
        .map_err(|err| SandboxMetadataError::Sandbox(err.display_with_causes()))?;
    if result.exit_code == 0 {
        Ok(result.stdout.trim().to_string())
    } else {
        Err(SandboxMetadataError::Git(exec_err(command, &result)))
    }
}

async fn exec_ok(
    sandbox: &dyn Sandbox,
    command: &str,
    env: Option<&HashMap<String, String>>,
) -> Result<(), SandboxMetadataError> {
    exec_stdout(sandbox, command, env).await.map(|_| ())
}

fn exec_err(label: &str, result: &fabro_sandbox::ExecResult) -> String {
    if result.timed_out {
        return format!("{label} timed out after {}ms", result.duration_ms);
    }
    let detail = format!("{}{}", result.stdout, result.stderr);
    let detail = detail.trim();
    if detail.is_empty() {
        format!("{label} failed with exit {}", result.exit_code)
    } else {
        format!("{label} failed with exit {}: {detail}", result.exit_code)
    }
}

fn validate_metadata_path(path: &str) -> Result<(), SandboxMetadataError> {
    let invalid = path.is_empty()
        || path.starts_with('/')
        || path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..");
    if invalid {
        return Err(SandboxMetadataError::Git(format!(
            "invalid metadata path: {path}"
        )));
    }
    Ok(())
}
