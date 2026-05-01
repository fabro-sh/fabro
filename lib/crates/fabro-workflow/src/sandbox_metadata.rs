use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use fabro_agent::Sandbox;
use fabro_sandbox::shell_quote;
use fabro_types::run_event::{MetadataSnapshotFailureKind, MetadataSnapshotPhase};
use fabro_util::time::elapsed_ms;
use tokio::fs;
use tokio::sync::OnceCell;

use crate::event::{Emitter, Event, StageScope};
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
    #[error("{message}")]
    Operation {
        message:          String,
        exec_output_tail: Option<fabro_types::ExecOutputTail>,
    },
}

impl SandboxMetadataError {
    pub(crate) fn exec_output_tail(&self) -> Option<fabro_types::ExecOutputTail> {
        match self {
            Self::Operation {
                exec_output_tail, ..
            } => exec_output_tail.clone(),
            _ => None,
        }
    }
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
    pub commit_sha:  String,
    pub push_error:  Option<SandboxMetadataError>,
    pub entry_count: usize,
    pub bytes:       u64,
}

/// Full payload for a `MetadataSnapshotFailed` event, grouped so the emit
/// helper stays under clippy's argument threshold.
#[derive(Debug)]
pub(crate) struct MetadataSnapshotFailure {
    pub kind:             MetadataSnapshotFailureKind,
    pub error:            String,
    pub causes:           Vec<String>,
    pub commit_sha:       Option<String>,
    pub entry_count:      Option<usize>,
    pub bytes:            Option<u64>,
    pub exec_output_tail: Option<fabro_types::ExecOutputTail>,
}

pub(crate) fn emit_metadata_snapshot_failed(
    emitter: &Emitter,
    phase: MetadataSnapshotPhase,
    branch: &str,
    started: Instant,
    failure: MetadataSnapshotFailure,
    scope: Option<&StageScope>,
) {
    let event = Event::MetadataSnapshotFailed {
        phase,
        branch: branch.to_string(),
        duration_ms: elapsed_ms(started),
        failure_kind: failure.kind,
        error: failure.error,
        causes: failure.causes,
        commit_sha: failure.commit_sha,
        entry_count: failure.entry_count,
        bytes: failure.bytes,
        exec_output_tail: failure.exec_output_tail,
    };
    if let Some(scope) = scope {
        emitter.emit_scoped(&event, scope);
    } else {
        emitter.emit(&event);
    }
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
        let entry_count = entries.len();
        let bytes = metadata_entries_bytes(&entries);
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

        let result = self
            .write_snapshot_in_temp(&entries, message, &temp, entry_count, bytes)
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
        entry_count: usize,
        bytes: u64,
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
            .map_err(|err| SandboxMetadataError::Operation {
                message:          err.display_with_causes(),
                exec_output_tail: err.default_redacted_output_tail(),
            })?;

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
        let push_error = self.sandbox.git_push_ref(&refspec).await.err().map(|err| {
            SandboxMetadataError::Operation {
                message:          err.display_with_causes(),
                exec_output_tail: err.default_redacted_output_tail(),
            }
        });
        Ok(MetadataSnapshot {
            commit_sha: commit,
            push_error,
            entry_count,
            bytes,
        })
    }
}

fn metadata_entries_bytes(entries: &[(String, Vec<u8>)]) -> u64 {
    entries.iter().fold(0, |total, (_, bytes)| {
        total.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
    })
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
        .ok_or_else(|| SandboxMetadataError::Operation {
            message:          format!(
                "git fast-import did not report imported commit mark (stdout_bytes={})",
                stdout.len()
            ),
            exec_output_tail: stdout_output_tail(stdout),
        })
}

fn stdout_output_tail(stdout: &str) -> Option<fabro_types::ExecOutputTail> {
    fabro_sandbox::ExecResult {
        stdout:      stdout.to_string(),
        stderr:      String::new(),
        exit_code:   Some(0),
        termination: fabro_types::CommandTermination::Exited,
        duration_ms: 0,
    }
    .default_redacted_output_tail()
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
        .map_err(|err| SandboxMetadataError::Operation {
            message:          err.display_with_causes(),
            exec_output_tail: err.default_redacted_output_tail(),
        })?;
    if result.is_success() {
        Ok(result.stdout.trim().to_string())
    } else {
        let error = result.into_exec_error(command.to_string());
        Err(SandboxMetadataError::Operation {
            message:          error.display_with_causes(),
            exec_output_tail: error.default_redacted_output_tail(),
        })
    }
}

async fn exec_ok(
    sandbox: &dyn Sandbox,
    command: &str,
    env: Option<&HashMap<String, String>>,
) -> Result<(), SandboxMetadataError> {
    exec_stdout(sandbox, command, env).await.map(|_| ())
}

fn validate_metadata_path(path: &str) -> Result<(), SandboxMetadataError> {
    let invalid = path.is_empty()
        || path.starts_with('/')
        || path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..");
    if invalid {
        return Err(SandboxMetadataError::Operation {
            message:          format!("invalid metadata path: {path}"),
            exec_output_tail: None,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use fabro_sandbox::{DirEntry, ExecResult, GrepOptions, Sandbox};
    use fabro_types::CommandTermination;
    use tokio_util::sync::CancellationToken;

    use super::*;

    struct ExecOnlySandbox {
        result: ExecResult,
    }

    #[async_trait]
    impl Sandbox for ExecOnlySandbox {
        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<usize>,
            _limit: Option<usize>,
        ) -> fabro_sandbox::Result<String> {
            unreachable!("read_file is not used by exec_stdout")
        }

        async fn write_file(&self, _path: &str, _content: &str) -> fabro_sandbox::Result<()> {
            unreachable!("write_file is not used by exec_stdout")
        }

        async fn delete_file(&self, _path: &str) -> fabro_sandbox::Result<()> {
            unreachable!("delete_file is not used by exec_stdout")
        }

        async fn file_exists(&self, _path: &str) -> fabro_sandbox::Result<bool> {
            unreachable!("file_exists is not used by exec_stdout")
        }

        async fn list_directory(
            &self,
            _path: &str,
            _depth: Option<usize>,
        ) -> fabro_sandbox::Result<Vec<DirEntry>> {
            unreachable!("list_directory is not used by exec_stdout")
        }

        async fn exec_command(
            &self,
            _command: &str,
            _timeout_ms: u64,
            _working_dir: Option<&str>,
            _env_vars: Option<&HashMap<String, String>>,
            _cancel_token: Option<CancellationToken>,
        ) -> fabro_sandbox::Result<ExecResult> {
            Ok(self.result.clone())
        }

        async fn grep(
            &self,
            _pattern: &str,
            _path: &str,
            _options: &GrepOptions,
        ) -> fabro_sandbox::Result<Vec<String>> {
            unreachable!("grep is not used by exec_stdout")
        }

        async fn glob(
            &self,
            _pattern: &str,
            _path: Option<&str>,
        ) -> fabro_sandbox::Result<Vec<String>> {
            unreachable!("glob is not used by exec_stdout")
        }

        async fn download_file_to_local(
            &self,
            _remote_path: &str,
            _local_path: &std::path::Path,
        ) -> fabro_sandbox::Result<()> {
            unreachable!("download_file_to_local is not used by exec_stdout")
        }

        async fn upload_file_from_local(
            &self,
            _local_path: &std::path::Path,
            _remote_path: &str,
        ) -> fabro_sandbox::Result<()> {
            unreachable!("upload_file_from_local is not used by exec_stdout")
        }

        async fn initialize(&self) -> fabro_sandbox::Result<()> {
            Ok(())
        }

        async fn cleanup(&self) -> fabro_sandbox::Result<()> {
            Ok(())
        }

        fn working_directory(&self) -> &str {
            "/work"
        }

        fn platform(&self) -> &str {
            "linux"
        }

        fn os_version(&self) -> String {
            "Linux".to_string()
        }
    }

    #[tokio::test]
    async fn metadata_snapshot_exec_stdout_error_carries_projected_tail() {
        let sandbox = ExecOnlySandbox {
            result: ExecResult {
                stdout:      "push stdout".to_string(),
                stderr:      "remote: Permission denied".to_string(),
                exit_code:   Some(128),
                termination: CommandTermination::Exited,
                duration_ms: 7,
            },
        };

        let err = exec_stdout(&sandbox, "git push origin refs/heads/run", None)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("git push origin"));
        assert_eq!(
            err.exec_output_tail()
                .as_ref()
                .and_then(|tail| tail.stderr.as_deref()),
            Some("remote: Permission denied")
        );
    }

    #[test]
    fn parse_fast_import_mark_error_is_log_safe_and_carries_projected_tail() {
        let secret = "sk-ant-api03-xK9mZ2vL8nQ5rT1wY4bC7dF0gH3jE6pA";
        let stdout = format!("unexpected output {secret}\n\u{1b}[31mcolored\u{1b}[0m");

        let err = parse_fast_import_mark(&stdout).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("stdout_bytes="));
        assert!(!message.contains("unexpected output"));
        assert!(!message.contains(secret));

        let stdout_tail = err
            .exec_output_tail()
            .and_then(|tail| tail.stdout)
            .expect("stdout tail");
        assert!(stdout_tail.contains("REDACTED"), "{stdout_tail}");
        assert!(stdout_tail.contains("colored"), "{stdout_tail}");
        assert!(!stdout_tail.contains(secret), "{stdout_tail}");
        assert!(!stdout_tail.contains('\u{1b}'), "{stdout_tail}");
    }
}
