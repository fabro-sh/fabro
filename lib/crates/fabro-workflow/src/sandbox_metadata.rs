use std::collections::HashMap;
use std::fmt::Write as _;
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
    pub push_error: Option<String>,
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
        exec_ok(
            self.sandbox,
            &format!(
                "rm -rf {temp_q} && mkdir -p {temp_q}",
                temp_q = shell_quote(&temp)
            ),
            None,
        )
        .await?;

        let result = self.write_snapshot_in_temp(&entries, message, &temp).await;
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
    ) -> Result<MetadataSnapshot, SandboxMetadataError> {
        let full_ref = format!("refs/heads/{}", self.branch);
        let old_commit = exec_stdout(
            self.sandbox,
            &format!(
                "{GIT_REMOTE} rev-parse --verify -q {}^{{commit}} || true",
                shell_quote(&full_ref)
            ),
            None,
        )
        .await?;
        let old_commit = (!old_commit.is_empty()).then_some(old_commit);

        let mut commit_message = message.to_string();
        self.git_author.append_footer(&mut commit_message);
        let stream = fast_import_stream(
            &full_ref,
            old_commit.as_deref(),
            &commit_message,
            entries,
            &self.git_author,
        )?;

        let local = tempfile::NamedTempFile::new().map_err(SandboxMetadataError::LocalTemp)?;
        fs::write(local.path(), stream)
            .await
            .map_err(SandboxMetadataError::LocalTemp)?;
        let remote = format!("{temp}/metadata.fi");
        self.sandbox
            .upload_file_from_local(local.path(), &remote)
            .await
            .map_err(|err| SandboxMetadataError::Sandbox(err.display_with_causes()))?;

        let stdout = exec_stdout(
            self.sandbox,
            &format!(
                "{GIT_REMOTE} fast-import --date-format=now < {}",
                shell_quote(&remote)
            ),
            None,
        )
        .await?;
        let commit = parse_fast_import_mark(&stdout)?;
        let refspec = format!("{full_ref}:{full_ref}");
        let push_error = self
            .sandbox
            .git_push_ref(&refspec)
            .await
            .err()
            .map(|err| err.to_string());
        Ok(MetadataSnapshot {
            commit_sha: commit,
            push_error,
        })
    }
}

fn fast_import_stream(
    full_ref: &str,
    old_commit: Option<&str>,
    commit_message: &str,
    entries: &[(String, Vec<u8>)],
    author: &GitAuthor,
) -> Result<Vec<u8>, SandboxMetadataError> {
    let mut stream = Vec::new();
    push_line(&mut stream, &format!("commit {full_ref}"));
    push_line(&mut stream, "mark :1");
    push_line(
        &mut stream,
        &format!("author {}", fast_import_ident(author)),
    );
    push_line(
        &mut stream,
        &format!("committer {}", fast_import_ident(author)),
    );
    push_data(&mut stream, commit_message.as_bytes());
    if let Some(old_commit) = old_commit {
        push_line(&mut stream, &format!("from {old_commit}"));
    }
    push_line(&mut stream, "deleteall");

    for (path, bytes) in entries {
        validate_metadata_path(path)?;
        push_line(
            &mut stream,
            &format!("M 100644 inline {}", fast_import_quote_path(path)),
        );
        push_data(&mut stream, bytes);
    }

    push_line(&mut stream, "get-mark :1");
    Ok(stream)
}

fn push_line(stream: &mut Vec<u8>, line: &str) {
    stream.extend_from_slice(line.as_bytes());
    stream.push(b'\n');
}

fn push_data(stream: &mut Vec<u8>, data: &[u8]) {
    push_line(stream, &format!("data {}", data.len()));
    stream.extend_from_slice(data);
    stream.push(b'\n');
}

fn parse_fast_import_mark(stdout: &str) -> Result<String, SandboxMetadataError> {
    stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(ToString::to_string)
        .ok_or_else(|| {
            SandboxMetadataError::Git(format!(
                "git fast-import did not report imported commit mark: {stdout:?}"
            ))
        })
}

fn fast_import_ident(author: &GitAuthor) -> String {
    let name = author
        .name
        .replace(['\n', '\r', '<', '>'], " ")
        .trim()
        .to_string();
    let name = if name.is_empty() {
        GitAuthor::default().name
    } else {
        name
    };
    let email = author
        .email
        .replace(['\n', '\r', '<', '>'], "")
        .trim()
        .to_string();
    let email = if email.is_empty() {
        GitAuthor::default().email
    } else {
        email
    };
    format!("{name} <{email}> now")
}

fn fast_import_quote_path(path: &str) -> String {
    if path
        .bytes()
        .all(|byte| byte > b' ' && byte != b'"' && byte != b'\\')
    {
        return path.to_string();
    }

    let mut quoted = String::from("\"");
    for byte in path.bytes() {
        match byte {
            b'\\' => quoted.push_str("\\\\"),
            b'"' => quoted.push_str("\\\""),
            b'\n' => quoted.push_str("\\n"),
            b'\r' => quoted.push_str("\\r"),
            b'\t' => quoted.push_str("\\t"),
            b' '..=b'~' => quoted.push(byte as char),
            _ => {
                let _ = write!(quoted, "\\{byte:03o}");
            }
        }
    }
    quoted.push('"');
    quoted
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
