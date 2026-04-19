use std::path::Path;

use fabro_agent::Sandbox;
use fabro_checkpoint::trailer as trailerlink;
use fabro_checkpoint::trailer::Trailer;
use fabro_sandbox::daytona::detect_repo_info;
use fabro_types::RunId;

use crate::artifact_snapshot;
use crate::git::{GitAuthor, blocking_push_with_timeout, push_ref};

/// Captured git state for a workflow run, shared with handlers.
#[derive(Debug, Clone)]
pub struct GitState {
    pub run_id:                   RunId,
    pub base_sha:                 String,
    pub run_branch:               Option<String>,
    pub meta_branch:              Option<String>,
    pub checkpoint_exclude_globs: Vec<String>,
    pub git_author:               GitAuthor,
}

pub const GIT_REMOTE: &str = "git -c maintenance.auto=0 -c gc.auto=0";

/// Shell-escape a string using `shlex::try_quote` (POSIX-safe).
fn shell_quote(s: &str) -> String {
    shlex::try_quote(s).map_or_else(
        |_| format!("'{}'", s.replace('\'', "'\\''")),
        |q| q.to_string(),
    )
}

/// Run a git checkpoint commit via the sandbox.
#[allow(clippy::too_many_arguments)]
pub async fn git_checkpoint(
    sandbox: &dyn Sandbox,
    run_id: &str,
    node_id: &str,
    status: &str,
    completed_count: usize,
    shadow_sha: Option<String>,
    exclude_globs: &[String],
    author: &GitAuthor,
) -> std::result::Result<String, String> {
    let mut all_excludes: Vec<String> = artifact_snapshot::EXCLUDE_DIRS
        .iter()
        .map(|d| format!("**/{d}/**"))
        .collect();
    all_excludes.extend(exclude_globs.iter().cloned());

    let pathspecs: Vec<String> = all_excludes
        .iter()
        .map(|g| format!("':(glob,exclude){g}'"))
        .collect();
    let add_cmd = format!("{GIT_REMOTE} add -A -- . {}", pathspecs.join(" "));
    let add_result = sandbox
        .exec_command(&add_cmd, 30_000, None, None, None)
        .await;
    match &add_result {
        Ok(r) if r.exit_code == 0 => {}
        Ok(r) => {
            return Err(format!(
                "git add failed (exit {}): {}{}",
                r.exit_code, r.stdout, r.stderr
            ));
        }
        Err(e) => return Err(format!("git add failed: {e}")),
    }

    let subject = format!("fabro({run_id}): {node_id} ({status})");
    let completed_str = completed_count.to_string();
    let mut trailers = vec![
        Trailer {
            key:   "Fabro-Run",
            value: run_id,
        },
        Trailer {
            key:   "Fabro-Completed",
            value: &completed_str,
        },
    ];
    let shadow_sha_ref = shadow_sha.as_deref().unwrap_or("");
    if shadow_sha.is_some() {
        trailers.push(Trailer {
            key:   "Fabro-Checkpoint",
            value: shadow_sha_ref,
        });
    }
    let mut message = trailerlink::format_message(&subject, "", &trailers);
    author.append_footer(&mut message);

    let msg_path = format!("/tmp/fabro-commit-msg-{run_id}-{node_id}");
    if let Err(e) = sandbox.write_file(&msg_path, &message).await {
        return Err(format!("failed to write commit message file: {e}"));
    }

    let commit_cmd = format!(
        "{GIT_REMOTE} -c user.name={name} -c user.email={email} commit --allow-empty -F {msg_path}",
        name = shell_quote(&author.name),
        email = shell_quote(&author.email),
    );
    let commit_result = sandbox
        .exec_command(&commit_cmd, 30_000, None, None, None)
        .await;
    match &commit_result {
        Ok(r) if r.exit_code == 0 => {}
        Ok(r) => {
            return Err(format!(
                "git commit failed (exit {}): {}{}",
                r.exit_code, r.stdout, r.stderr
            ));
        }
        Err(e) => return Err(format!("git commit failed: {e}")),
    }

    let sha_cmd = format!("{GIT_REMOTE} rev-parse HEAD");
    let sha_result = sandbox
        .exec_command(&sha_cmd, 10_000, None, None, None)
        .await;
    match sha_result {
        Ok(r) if r.exit_code == 0 => Ok(r.stdout.trim().to_string()),
        Ok(r) => Err(format!(
            "git rev-parse HEAD failed (exit {}): {}{}",
            r.exit_code, r.stdout, r.stderr
        )),
        Err(e) => Err(format!("git rev-parse HEAD failed: {e}")),
    }
}

/// Push a refspec from the host repo to origin (best-effort).
///
/// Authenticates via resolved GitHub credentials so we don't depend on the
/// host's ambient git credentials.
pub async fn git_push_host(
    repo_path: &Path,
    refspec: &str,
    github_app: &Option<fabro_github::GitHubCredentials>,
    label: &str,
) -> bool {
    let (origin_url, _) = match detect_repo_info(repo_path) {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!(error = %e, label, "Cannot detect origin for push");
            return false;
        }
    };

    let https_url = fabro_github::ssh_url_to_https(&origin_url);
    let push_url = if let Some(creds) = github_app {
        match fabro_github::resolve_authenticated_url(
            creds,
            &https_url,
            &fabro_github::github_api_base_url(),
        )
        .await
        {
            Ok(url) => url,
            Err(e) => {
                tracing::warn!(error = %e, label, "Failed to get token for push");
                return false;
            }
        }
    } else {
        tracing::warn!(label, "No GitHub credentials for push");
        return false;
    };

    let rp = repo_path.to_path_buf();
    let refspec_owned = refspec.to_string();
    let result =
        blocking_push_with_timeout(60, move || push_ref(&rp, &push_url, &refspec_owned)).await;
    match result {
        Ok(()) => {
            tracing::info!(label, "Pushed to origin");
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, label, "Failed to push");
            false
        }
    }
}

/// Run a git diff via the sandbox (30 s default timeout).
pub(crate) async fn git_diff(
    sandbox: &dyn Sandbox,
    base: &str,
) -> std::result::Result<String, String> {
    git_diff_with_timeout(sandbox, base, 30_000).await
}

/// Run a git diff via the sandbox with a caller-supplied timeout in
/// milliseconds.
///
/// Failure-path capture uses a shorter timeout than the checkpoint path so a
/// pathological workspace (FS locks, corrupted index) doesn't stall terminal
/// event emission downstream (Slack notifier, SSE, CI hooks).
pub(crate) async fn git_diff_with_timeout(
    sandbox: &dyn Sandbox,
    base: &str,
    timeout_ms: u64,
) -> std::result::Result<String, String> {
    // `-c core.quotePath=false` forces paths with non-ASCII, tabs, quotes,
    // or backslashes to emit unquoted. The Run Files Changed endpoint's
    // `strip_denylisted_sections` parser only recognizes unquoted
    // `diff --git a/<old> b/<new>` headers; without this flag git would
    // wrap such paths in `"a/…"` / `"b/…"` and evade the denylist (see
    // docs/agent/reviews/2026-04-19-run-files-security-review.md).
    let cmd = format!("{GIT_REMOTE} -c core.quotePath=false diff {base} HEAD");
    match sandbox
        .exec_command(&cmd, timeout_ms, None, None, None)
        .await
    {
        Ok(r) if r.exit_code == 0 => Ok(r.stdout),
        Ok(r) => Err(format!("exit {}: {}", r.exit_code, r.stderr.trim())),
        Err(e) => Err(e.clone()),
    }
}

/// Create a branch at a specific SHA via the sandbox.
pub async fn git_create_branch_at(sandbox: &dyn Sandbox, name: &str, sha: &str) -> bool {
    let cmd = format!("{GIT_REMOTE} branch --force {name} {sha}");
    matches!(
        sandbox.exec_command(&cmd, 30_000, None, None, None).await,
        Ok(r) if r.exit_code == 0
    )
}

/// Add a git worktree via the sandbox.
pub async fn git_add_worktree(sandbox: &dyn Sandbox, path: &str, branch: &str) -> bool {
    let cmd = format!("{GIT_REMOTE} worktree add {path} {branch}");
    matches!(
        sandbox.exec_command(&cmd, 30_000, None, None, None).await,
        Ok(r) if r.exit_code == 0
    )
}

/// Remove a git worktree via the sandbox.
pub async fn git_remove_worktree(sandbox: &dyn Sandbox, path: &str) -> bool {
    let cmd = format!("{GIT_REMOTE} worktree remove --force {path}");
    matches!(
        sandbox.exec_command(&cmd, 30_000, None, None, None).await,
        Ok(r) if r.exit_code == 0
    )
}

/// Fast-forward merge to a given SHA via the sandbox.
pub async fn git_merge_ff_only(sandbox: &dyn Sandbox, sha: &str) -> bool {
    let cmd = format!("{GIT_REMOTE} merge --ff-only {sha}");
    matches!(
        sandbox.exec_command(&cmd, 30_000, None, None, None).await,
        Ok(r) if r.exit_code == 0
    )
}

/// Remove any stale worktree at `path` (best-effort), then add a fresh one.
pub async fn git_replace_worktree(sandbox: &dyn Sandbox, path: &str, branch: &str) -> bool {
    let _ = git_remove_worktree(sandbox, path).await;
    git_add_worktree(sandbox, path, branch).await
}

// ── Machine-readable diff enumeration (Run Files endpoint) ─────────────────

/// Hardened git-command prefix for the Run Files endpoint.
///
/// Layers on top of [`GIT_REMOTE`]:
/// - `core.hooksPath=/dev/null`: repo-supplied hooks do not run.
/// - `core.fsmonitor=false`: no fsmonitor daemon interactions.
/// - `protocol.file.allow=never`: blocks local-protocol fetches.
///
/// These invocations use [`sandbox_git_hardening_env`] via `exec_command` to
/// disable terminal prompts and external diff drivers.
const GIT_HARDENED: &str = "git -c maintenance.auto=0 -c gc.auto=0 -c core.hooksPath=/dev/null -c core.fsmonitor=false -c protocol.file.allow=never -c core.quotePath=false";

/// Environment additions applied to every hardened sandbox-side git invocation.
///
/// `GIT_TERMINAL_PROMPT=0` prevents git from stalling on credential prompts
/// when a remote or subprocess triggers one. Clearing `GIT_EXTERNAL_DIFF`
/// neutralizes any inherited custom diff driver.
fn sandbox_git_hardening_env() -> std::collections::HashMap<String, String> {
    std::collections::HashMap::from([
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        ("GIT_EXTERNAL_DIFF".to_string(), String::new()),
    ])
}

/// A single changed-file entry from `git diff --raw -z --find-renames=50%`.
///
/// Paths are repo-relative, UTF-8; non-UTF-8 filenames are rejected by the
/// parser. Blob SHAs are lowercase hex. Modes are octal integers (`100644`,
/// `100755`, `120000`, `160000`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawDiffEntry {
    Added {
        path:     String,
        new_blob: String,
        new_mode: String,
    },
    Modified {
        path:     String,
        old_blob: String,
        new_blob: String,
        new_mode: String,
    },
    Deleted {
        path:     String,
        old_blob: String,
        old_mode: String,
    },
    Renamed {
        old_path:   String,
        new_path:   String,
        old_blob:   String,
        new_blob:   String,
        new_mode:   String,
        similarity: u8,
    },
    Symlink {
        path:        String,
        change_kind: SymlinkChange,
        old_blob:    Option<String>,
        new_blob:    Option<String>,
    },
    Submodule {
        path:        String,
        change_kind: SubmoduleChange,
    },
}

/// Lifecycle of a symlink entry (mode `120000`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymlinkChange {
    Added,
    Modified,
    Deleted,
}

/// Lifecycle of a submodule entry (mode `160000`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmoduleChange {
    Added,
    Modified,
    Deleted,
}

/// Error produced by the sandbox-git helpers.
///
/// Callers discriminate between transient (retry-safe) and permanent
/// conditions: a 503 can be returned to the client on `Transient`, while
/// `Permanent` errors should fall through to the patch-only fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffError {
    /// Retry-safe failure: timeout, process kill, transient I/O.
    Transient { message: String },
    /// Non-retryable failure: unknown revision, malformed output, etc.
    Permanent { message: String },
}

impl std::fmt::Display for DiffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transient { message } => write!(f, "transient: {message}"),
            Self::Permanent { message } => write!(f, "permanent: {message}"),
        }
    }
}

impl std::error::Error for DiffError {}

/// Size metadata for a single blob, as reported by `git cat-file
/// --batch-check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobMeta {
    pub sha:  String,
    /// `None` if the blob is missing (git reports `missing`).
    pub size: Option<u64>,
}

/// Enumerate files changed between `base_sha` and `to_sha` via the sandbox.
///
/// Uses `git diff --raw -z --find-renames=50%` to get a machine-readable,
/// null-separated, SHA-addressed listing. Paths from this output are treated
/// as metadata only — blob reads use the SHAs, not the paths.
///
/// The `--numstat` side-call classifies text vs binary so callers can skip
/// binary contents without ever invoking `git cat-file --batch` on them.
pub async fn list_changed_files_raw(
    sandbox: &dyn Sandbox,
    base_sha: &str,
    to_sha: &str,
) -> std::result::Result<Vec<RawDiffEntry>, DiffError> {
    let base_q = shell_quote(base_sha);
    let to_q = shell_quote(to_sha);
    let env = sandbox_git_hardening_env();
    let cmd = format!("{GIT_HARDENED} diff --raw -z --find-renames=50% {base_q}..{to_q}");
    let res = sandbox
        .exec_command(&cmd, 10_000, None, Some(&env), None)
        .await
        .map_err(|e| DiffError::Transient { message: e })?;

    if res.timed_out {
        return Err(DiffError::Transient {
            message: "git diff --raw timed out".to_string(),
        });
    }
    if res.exit_code != 0 {
        // An unknown-object / bad-revision error is permanent; everything
        // else we treat as transient so the server can retry safely.
        let stderr = res.stderr.trim().to_string();
        if is_permanent_git_error(&stderr) {
            return Err(DiffError::Permanent { message: stderr });
        }
        return Err(DiffError::Transient { message: stderr });
    }

    parse_raw_z(&res.stdout).map_err(|message| DiffError::Permanent { message })
}

fn is_permanent_git_error(stderr: &str) -> bool {
    // git emits these to stderr for unknown revisions / missing objects;
    // treat them as Permanent so the handler falls through to final_patch.
    let lower = stderr.to_lowercase();
    lower.contains("unknown revision")
        || lower.contains("bad revision")
        || lower.contains("invalid revision")
        || lower.contains("no such path")
        || lower.contains("not a valid object name")
}

fn parse_raw_z(stdout: &str) -> std::result::Result<Vec<RawDiffEntry>, String> {
    // git diff --raw -z format:
    //   ":<srcmode> <dstmode> <srcsha> <dstsha> <status>\0<path>\0"
    // For renames/copies:
    //   ":<srcmode> <dstmode> <srcsha> <dstsha> R<score>\0<oldpath>\0<newpath>\0"
    //
    // Multiple entries are concatenated with no separator between them.
    let mut entries = Vec::new();
    let mut tokens = stdout.split('\0').peekable();
    while let Some(header) = tokens.next() {
        if header.is_empty() {
            continue;
        }
        if !header.starts_with(':') {
            return Err(format!("unexpected token in diff --raw: {header:?}"));
        }
        let fields: Vec<&str> = header[1..].split(' ').collect();
        if fields.len() < 5 {
            return Err(format!("short raw-diff header: {header:?}"));
        }
        let src_mode = fields[0];
        let dst_mode = fields[1];
        let src_sha = fields[2];
        let dst_sha = fields[3];
        let status = fields[4];

        let entry = if status.starts_with('R') || status.starts_with('C') {
            let score: u8 = status[1..].parse().unwrap_or(0);
            let old_path = tokens
                .next()
                .ok_or_else(|| "missing old_path for rename".to_string())?
                .to_string();
            let new_path = tokens
                .next()
                .ok_or_else(|| "missing new_path for rename".to_string())?
                .to_string();
            RawDiffEntry::Renamed {
                old_path,
                new_path,
                old_blob: src_sha.to_string(),
                new_blob: dst_sha.to_string(),
                new_mode: dst_mode.to_string(),
                similarity: score,
            }
        } else {
            let path = tokens
                .next()
                .ok_or_else(|| "missing path for diff entry".to_string())?
                .to_string();
            classify_entry(status, src_mode, dst_mode, src_sha, dst_sha, &path)?
        };
        entries.push(entry);
    }
    Ok(entries)
}

fn classify_entry(
    status: &str,
    src_mode: &str,
    dst_mode: &str,
    src_sha: &str,
    dst_sha: &str,
    path: &str,
) -> std::result::Result<RawDiffEntry, String> {
    // Mode 120000 = symlink, 160000 = submodule (gitlink).
    let is_symlink_change = src_mode == "120000" || dst_mode == "120000";
    let is_submodule_change = src_mode == "160000" || dst_mode == "160000";

    Ok(match (status, is_symlink_change, is_submodule_change) {
        ("A", true, _) => RawDiffEntry::Symlink {
            path:        path.to_string(),
            change_kind: SymlinkChange::Added,
            old_blob:    None,
            new_blob:    Some(dst_sha.to_string()),
        },
        ("A", _, true) => RawDiffEntry::Submodule {
            path:        path.to_string(),
            change_kind: SubmoduleChange::Added,
        },
        ("A", _, _) => RawDiffEntry::Added {
            path:     path.to_string(),
            new_blob: dst_sha.to_string(),
            new_mode: dst_mode.to_string(),
        },
        ("D", true, _) => RawDiffEntry::Symlink {
            path:        path.to_string(),
            change_kind: SymlinkChange::Deleted,
            old_blob:    Some(src_sha.to_string()),
            new_blob:    None,
        },
        ("D", _, true) => RawDiffEntry::Submodule {
            path:        path.to_string(),
            change_kind: SubmoduleChange::Deleted,
        },
        ("D", _, _) => RawDiffEntry::Deleted {
            path:     path.to_string(),
            old_blob: src_sha.to_string(),
            old_mode: src_mode.to_string(),
        },
        ("M" | "T", true, _) => RawDiffEntry::Symlink {
            path:        path.to_string(),
            change_kind: SymlinkChange::Modified,
            old_blob:    Some(src_sha.to_string()),
            new_blob:    Some(dst_sha.to_string()),
        },
        ("M" | "T", _, true) => RawDiffEntry::Submodule {
            path:        path.to_string(),
            change_kind: SubmoduleChange::Modified,
        },
        ("M" | "T", _, _) => RawDiffEntry::Modified {
            path:     path.to_string(),
            old_blob: src_sha.to_string(),
            new_blob: dst_sha.to_string(),
            new_mode: dst_mode.to_string(),
        },
        (other, _, _) => {
            return Err(format!("unknown raw-diff status {other:?} for {path:?}"));
        }
    })
}

/// Classify which changed paths in `base_sha..to_sha` are binary, via
/// `git diff --numstat`. Binary entries show `-\t-\t<path>` in the output.
///
/// Returns the set of repo-relative paths (post-rename, i.e. `<new_path>`
/// when a rename occurred) that `git` classifies as binary.
pub async fn list_binary_paths(
    sandbox: &dyn Sandbox,
    base_sha: &str,
    to_sha: &str,
) -> std::result::Result<std::collections::HashSet<String>, DiffError> {
    let base_q = shell_quote(base_sha);
    let to_q = shell_quote(to_sha);
    let env = sandbox_git_hardening_env();
    let cmd = format!("{GIT_HARDENED} diff --numstat --find-renames=50% {base_q}..{to_q}");
    let res = sandbox
        .exec_command(&cmd, 10_000, None, Some(&env), None)
        .await
        .map_err(|e| DiffError::Transient { message: e })?;

    if res.timed_out {
        return Err(DiffError::Transient {
            message: "git diff --numstat timed out".to_string(),
        });
    }
    if res.exit_code != 0 {
        let stderr = res.stderr.trim().to_string();
        if is_permanent_git_error(&stderr) {
            return Err(DiffError::Permanent { message: stderr });
        }
        return Err(DiffError::Transient { message: stderr });
    }

    let mut binary = std::collections::HashSet::new();
    for line in res.stdout.lines() {
        // `-\t-\t<path>` marks binary. Rename lines read `<+>\t<->\t<path> =>
        // <path>` or `<+>\t<->\t{<old> => <new>}` — for the binary case we
        // just need presence of "-\t-" prefix; extract the path portion.
        if let Some(rest) = line.strip_prefix("-\t-\t") {
            // Normalize rename display to the new path.
            let path = extract_new_path_from_numstat(rest);
            binary.insert(path);
        }
    }
    Ok(binary)
}

fn extract_new_path_from_numstat(rest: &str) -> String {
    // Forms seen:
    //   "simple/path"
    //   "old => new"
    //   "prefix/{old => new}/suffix"
    if let Some(open_idx) = rest.find('{') {
        if let Some(close_idx) = rest[open_idx..].find('}') {
            let before = &rest[..open_idx];
            let after = &rest[open_idx + close_idx + 1..];
            let inside = &rest[open_idx + 1..open_idx + close_idx];
            if let Some((_, new)) = inside.split_once(" => ") {
                return format!("{before}{new}{after}");
            }
        }
    }
    if let Some((_, new)) = rest.split_once(" => ") {
        return new.to_string();
    }
    rest.to_string()
}

/// Fetch blob metadata (size) for many SHAs in one sandbox invocation via
/// `git cat-file --batch-check`.
///
/// The order of returned `BlobMeta` entries matches the input `shas` order.
/// SHAs reported as `missing` by git yield `BlobMeta { size: None, .. }`.
pub async fn stream_blob_metadata(
    sandbox: &dyn Sandbox,
    shas: &[String],
) -> std::result::Result<Vec<BlobMeta>, DiffError> {
    if shas.is_empty() {
        return Ok(Vec::new());
    }
    let env = sandbox_git_hardening_env();
    let quoted_shas: Vec<String> = shas.iter().map(|s| shell_quote(s)).collect();
    let cmd = format!(
        "printf '%s\\n' {} | {GIT_HARDENED} cat-file --batch-check",
        quoted_shas.join(" ")
    );
    let res = sandbox
        .exec_command(&cmd, 10_000, None, Some(&env), None)
        .await
        .map_err(|e| DiffError::Transient { message: e })?;

    if res.timed_out {
        return Err(DiffError::Transient {
            message: "git cat-file --batch-check timed out".to_string(),
        });
    }
    if res.exit_code != 0 {
        return Err(DiffError::Transient {
            message: format!("git cat-file --batch-check failed: {}", res.stderr.trim()),
        });
    }

    let mut metas = Vec::with_capacity(shas.len());
    for line in res.stdout.lines() {
        // Lines: "<sha> <type> <size>" OR "<sha> missing"
        let mut parts = line.split(' ');
        let sha = parts
            .next()
            .ok_or_else(|| DiffError::Permanent {
                message: format!("empty cat-file line: {line:?}"),
            })?
            .to_string();
        let second = parts.next().unwrap_or("");
        if second == "missing" {
            metas.push(BlobMeta { sha, size: None });
            continue;
        }
        let size_str = parts.next().unwrap_or("");
        let size = size_str.parse::<u64>().map_err(|e| DiffError::Permanent {
            message: format!("unparseable size {size_str:?} for {sha}: {e}"),
        })?;
        metas.push(BlobMeta {
            sha,
            size: Some(size),
        });
    }
    Ok(metas)
}

/// Fetch blob contents for many SHAs in one sandbox invocation via
/// `git cat-file --batch`.
///
/// Contents are size-capped per blob: any blob exceeding `size_cap_bytes`
/// returns `None` in its slot (the caller should flag that entry as
/// truncated). Callers are expected to have pre-filtered binary blobs via
/// [`list_binary_paths`] — `--batch` output stream is text-oriented and
/// non-UTF-8 bytes are lossy through the sandbox `String` channel.
pub async fn stream_blobs(
    sandbox: &dyn Sandbox,
    shas: &[String],
    size_cap_bytes: u64,
) -> std::result::Result<Vec<Option<String>>, DiffError> {
    if shas.is_empty() {
        return Ok(Vec::new());
    }
    let env = sandbox_git_hardening_env();
    let quoted_shas: Vec<String> = shas.iter().map(|s| shell_quote(s)).collect();
    let cmd = format!(
        "printf '%s\\n' {} | {GIT_HARDENED} cat-file --batch",
        quoted_shas.join(" ")
    );
    let res = sandbox
        .exec_command(&cmd, 10_000, None, Some(&env), None)
        .await
        .map_err(|e| DiffError::Transient { message: e })?;

    if res.timed_out {
        return Err(DiffError::Transient {
            message: "git cat-file --batch timed out".to_string(),
        });
    }
    if res.exit_code != 0 {
        return Err(DiffError::Transient {
            message: format!("git cat-file --batch failed: {}", res.stderr.trim()),
        });
    }

    parse_batch_output(&res.stdout, shas, size_cap_bytes)
        .map_err(|message| DiffError::Permanent { message })
}

fn parse_batch_output(
    stdout: &str,
    shas: &[String],
    size_cap_bytes: u64,
) -> std::result::Result<Vec<Option<String>>, String> {
    // `git cat-file --batch` output per blob:
    //   "<sha> <type> <size>\n<content-bytes>\n"
    // `missing` blob: "<sha> missing\n" (no content).
    let mut results: Vec<Option<String>> = Vec::with_capacity(shas.len());
    let bytes = stdout.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() {
        // Find end of header line.
        let Some(nl_rel) = bytes[pos..].iter().position(|&b| b == b'\n') else {
            break;
        };
        let header = std::str::from_utf8(&bytes[pos..pos + nl_rel])
            .map_err(|e| format!("non-utf8 header in cat-file output: {e}"))?;
        pos += nl_rel + 1;

        let mut parts = header.split(' ');
        let _sha = parts.next().unwrap_or("");
        let second = parts.next().unwrap_or("");
        if second == "missing" {
            results.push(None);
            continue;
        }
        let size_str = parts.next().unwrap_or("");
        let size: usize = size_str
            .parse()
            .map_err(|e| format!("unparseable size {size_str:?}: {e}"))?;

        let end = pos + size;
        if end > bytes.len() {
            return Err(format!(
                "cat-file stream truncated: expected {size} bytes, have {}",
                bytes.len() - pos
            ));
        }
        if (size as u64) > size_cap_bytes {
            results.push(None);
        } else {
            let content = std::str::from_utf8(&bytes[pos..end])
                .map_err(|e| format!("non-utf8 blob contents: {e}"))?;
            results.push(Some(content.to_string()));
        }
        pos = end;
        // Trailing newline that delimits the next entry.
        if pos < bytes.len() && bytes[pos] == b'\n' {
            pos += 1;
        }
    }

    // Pad with None if the stream didn't cover every requested SHA (e.g.
    // duplicate-sha deduping by git).
    while results.len() < shas.len() {
        results.push(None);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::disallowed_methods,
        reason = "These unit tests use the real git CLI to construct sandbox-git fixture repositories."
    )]

    use super::*;

    #[tokio::test]
    async fn git_checkpoint_includes_builtin_excludes() {
        // Set up a real git repo
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@test.com",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ])
            .current_dir(repo)
            .output()
            .unwrap();

        // Create files in both tracked and excluded directories
        std::fs::write(repo.join("hello.txt"), "hello").unwrap();
        std::fs::create_dir_all(repo.join("node_modules/pkg")).unwrap();
        std::fs::write(repo.join("node_modules/pkg/index.js"), "module").unwrap();
        std::fs::create_dir_all(repo.join(".venv/lib")).unwrap();
        std::fs::write(repo.join(".venv/lib/site.py"), "venv").unwrap();

        let sandbox = fabro_agent::LocalSandbox::new(repo.to_path_buf());
        let author = crate::git::GitAuthor::default();

        // Call git_checkpoint with empty user excludes — built-in excludes should still
        // apply
        let result =
            git_checkpoint(&sandbox, "run1", "work", "success", 1, None, &[], &author).await;
        assert!(result.is_ok(), "git_checkpoint failed: {:?}", result.err());

        // Verify that excluded directories were NOT staged
        let status = sandbox
            .exec_command(
                "git diff --cached --name-only HEAD~1",
                10_000,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let staged_files: Vec<&str> = status.stdout.lines().collect();
        assert!(
            staged_files.contains(&"hello.txt"),
            "expected hello.txt to be staged, got: {staged_files:?}"
        );
        assert!(
            !staged_files.iter().any(|f| f.contains("node_modules")),
            "node_modules should be excluded from checkpoint, got: {staged_files:?}"
        );
        assert!(
            !staged_files.iter().any(|f| f.contains(".venv")),
            ".venv should be excluded from checkpoint, got: {staged_files:?}"
        );
    }

    // Test helpers for machine-readable diff enumeration. The repo is seeded
    // with a single commit at `base_sha`, then callers mutate and re-commit
    // to produce a synthetic `base_sha..HEAD` diff.
    fn init_git_repo(repo: &std::path::Path) {
        let _ = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo)
            .output()
            .unwrap();
        let _ = std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .output()
            .unwrap();
        let _ = std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo)
            .output()
            .unwrap();
    }

    fn git_commit_all(repo: &std::path::Path, msg: &str) -> String {
        let _ = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(repo)
            .output()
            .unwrap();
        let _ = std::process::Command::new("git")
            .args(["commit", "-m", msg])
            .current_dir(repo)
            .output()
            .unwrap();
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[tokio::test]
    async fn list_changed_files_raw_classifies_add_modify_delete() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        init_git_repo(repo);

        std::fs::write(repo.join("keep.txt"), "v1\n").unwrap();
        std::fs::write(repo.join("drop.txt"), "goodbye\n").unwrap();
        let base = git_commit_all(repo, "initial");

        std::fs::write(repo.join("keep.txt"), "v2\n").unwrap();
        std::fs::write(repo.join("add.txt"), "new\n").unwrap();
        std::fs::remove_file(repo.join("drop.txt")).unwrap();
        let head = git_commit_all(repo, "change");

        let sandbox = fabro_agent::LocalSandbox::new(repo.to_path_buf());
        let entries = list_changed_files_raw(&sandbox, &base, &head)
            .await
            .unwrap();

        assert_eq!(entries.len(), 3, "entries: {entries:?}");
        assert!(
            entries
                .iter()
                .any(|e| matches!(e, RawDiffEntry::Added { path, .. } if path == "add.txt"))
        );
        assert!(
            entries
                .iter()
                .any(|e| matches!(e, RawDiffEntry::Modified { path, .. } if path == "keep.txt"))
        );
        assert!(
            entries
                .iter()
                .any(|e| matches!(e, RawDiffEntry::Deleted { path, .. } if path == "drop.txt"))
        );
    }

    #[tokio::test]
    async fn list_changed_files_raw_detects_rename_above_threshold() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        init_git_repo(repo);

        // Write a file with enough content that a rename remains >= 50%
        // similar even after the rename (identical content should be 100%).
        let content = "line of shared content\n".repeat(50);
        std::fs::write(repo.join("old.txt"), &content).unwrap();
        let base = git_commit_all(repo, "initial");

        std::fs::remove_file(repo.join("old.txt")).unwrap();
        std::fs::write(repo.join("new.txt"), &content).unwrap();
        let head = git_commit_all(repo, "rename");

        let sandbox = fabro_agent::LocalSandbox::new(repo.to_path_buf());
        let entries = list_changed_files_raw(&sandbox, &base, &head)
            .await
            .unwrap();

        let renames: Vec<_> = entries
            .iter()
            .filter_map(|e| match e {
                RawDiffEntry::Renamed {
                    old_path,
                    new_path,
                    similarity,
                    ..
                } => Some((old_path.clone(), new_path.clone(), *similarity)),
                _ => None,
            })
            .collect();
        assert_eq!(renames.len(), 1, "expected one rename, got: {entries:?}");
        let (old_path, new_path, similarity) = &renames[0];
        assert_eq!(old_path, "old.txt");
        assert_eq!(new_path, "new.txt");
        assert!(*similarity >= 50, "similarity = {similarity}");
    }

    #[tokio::test]
    async fn list_binary_paths_flags_png_but_not_text() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        init_git_repo(repo);

        std::fs::write(repo.join("doc.md"), "hi").unwrap();
        let base = git_commit_all(repo, "initial");

        std::fs::write(repo.join("doc.md"), "bye\n").unwrap();
        // Minimal PNG header (8-byte signature) + a chunk — git classifies
        // this as binary via NUL-byte detection.
        let png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H',
            b'D', b'R',
        ];
        std::fs::write(repo.join("logo.png"), png).unwrap();
        let head = git_commit_all(repo, "change");

        let sandbox = fabro_agent::LocalSandbox::new(repo.to_path_buf());
        let binary = list_binary_paths(&sandbox, &base, &head).await.unwrap();

        assert!(binary.contains("logo.png"), "binary: {binary:?}");
        assert!(!binary.contains("doc.md"), "binary: {binary:?}");
    }

    #[tokio::test]
    async fn stream_blob_metadata_returns_sizes_in_order() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        init_git_repo(repo);

        std::fs::write(repo.join("a.txt"), "aaa\n").unwrap();
        std::fs::write(repo.join("b.txt"), "bb\n").unwrap();
        git_commit_all(repo, "seed");

        let ls = std::process::Command::new("git")
            .args(["ls-files", "-s"])
            .current_dir(repo)
            .output()
            .unwrap();
        // `ls-files -s` format: "<mode> <sha> <stage>\t<path>"
        let mut sha_by_name = std::collections::HashMap::new();
        for line in String::from_utf8_lossy(&ls.stdout).lines() {
            let mut cols = line.splitn(2, '\t');
            let (meta, path) = (cols.next().unwrap(), cols.next().unwrap());
            let mut parts = meta.split_whitespace();
            let _mode = parts.next();
            let sha = parts.next().unwrap();
            sha_by_name.insert(path.to_string(), sha.to_string());
        }

        let sandbox = fabro_agent::LocalSandbox::new(repo.to_path_buf());
        let shas = vec![sha_by_name["a.txt"].clone(), sha_by_name["b.txt"].clone()];
        let metas = stream_blob_metadata(&sandbox, &shas).await.unwrap();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].sha, shas[0]);
        assert_eq!(metas[0].size, Some(4));
        assert_eq!(metas[1].sha, shas[1]);
        assert_eq!(metas[1].size, Some(3));
    }

    #[tokio::test]
    async fn stream_blobs_returns_contents_and_respects_size_cap() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        init_git_repo(repo);

        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        let big = "b".repeat(200);
        std::fs::write(repo.join("big.txt"), &big).unwrap();
        git_commit_all(repo, "seed");

        let ls = std::process::Command::new("git")
            .args(["ls-files", "-s"])
            .current_dir(repo)
            .output()
            .unwrap();
        let mut sha_by_name = std::collections::HashMap::new();
        for line in String::from_utf8_lossy(&ls.stdout).lines() {
            let mut cols = line.splitn(2, '\t');
            let (meta, path) = (cols.next().unwrap(), cols.next().unwrap());
            let mut parts = meta.split_whitespace();
            let _mode = parts.next();
            let sha = parts.next().unwrap();
            sha_by_name.insert(path.to_string(), sha.to_string());
        }

        let sandbox = fabro_agent::LocalSandbox::new(repo.to_path_buf());
        let shas = vec![sha_by_name["a.txt"].clone(), sha_by_name["big.txt"].clone()];

        // size_cap = 100 bytes — "hello\n" (6) stays, 200-byte blob truncates.
        let contents = stream_blobs(&sandbox, &shas, 100).await.unwrap();
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0].as_deref(), Some("hello\n"));
        assert!(contents[1].is_none(), "oversize blob should be None");
    }

    #[tokio::test]
    async fn list_changed_files_raw_bad_revision_is_permanent() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        init_git_repo(repo);
        std::fs::write(repo.join("x"), "x").unwrap();
        git_commit_all(repo, "seed");

        let sandbox = fabro_agent::LocalSandbox::new(repo.to_path_buf());
        let err =
            list_changed_files_raw(&sandbox, "0000000000000000000000000000000000000000", "HEAD")
                .await
                .expect_err("expected error for unknown base sha");
        assert!(matches!(err, DiffError::Permanent { .. }), "err: {err:?}");
    }

    #[test]
    fn extract_new_path_from_numstat_handles_brace_renames() {
        assert_eq!(extract_new_path_from_numstat("simple/path"), "simple/path");
        assert_eq!(
            extract_new_path_from_numstat("old.txt => new.txt"),
            "new.txt"
        );
        assert_eq!(
            extract_new_path_from_numstat("src/{old => new}/file.rs"),
            "src/new/file.rs"
        );
    }
}
