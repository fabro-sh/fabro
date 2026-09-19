//! Fabro's git operations on a run's sandbox, over the driver's git facet.
//!
//! The driver runs every command hardened (no auto maintenance or gc, no
//! repository hooks, no fsmonitor, unquoted paths, no signing; read verbs
//! refuse the file transport and external diff drivers) and returns typed
//! results. Fabro decides what to stage, what to say in a checkpoint
//! commit, and which ranges the Run Files endpoint reads.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use fabro_sandbox::RunSandbox;
use sandbox_driver::{
    Git as _, GitChange, GitDiffEntry, GitDiffOptions, GitFacet, GitFailureKind, GitRevisionRange,
};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct GitCommandError {
    pub message: String,
    #[source]
    pub source:  fabro_sandbox::Error,
}

/// Rename detection threshold for the diffs the Run Files endpoint and the
/// checkpoint summaries read.
const FIND_RENAMES_PERCENT: u8 = 50;

/// Budget for the machine-readable diffs behind the Run Files endpoint.
const RUN_FILES_TIMEOUT: Duration = Duration::from_secs(10);

// ── Machine-readable diff enumeration (Run Files endpoint) ─────────────────

/// A single changed-file entry of a range, as the Run Files endpoint reads
/// it.
///
/// Paths are repo-relative, UTF-8. Blob SHAs are lowercase hex. Modes are
/// octal strings (`100644`, `100755`, `120000`, `160000`, …).
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
    /// Symlink creation, deletion, or target change. No blob contents are
    /// fetched for these: the "content" is the link target, which is
    /// not meaningful to diff as file text.
    Symlink {
        path:        String,
        change_kind: SymlinkChange,
        old_blob:    Option<String>,
        new_blob:    Option<String>,
    },
    /// Submodule (gitlink) pointer change. No blob contents exist for
    /// these in the parent repo.
    Submodule {
        path:        String,
        change_kind: SubmoduleChange,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkChange {
    Added,
    Deleted,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmoduleChange {
    Added,
    Deleted,
    Modified,
}

/// Errors from the machine-readable diff paths, classified so the server
/// can fall back or retry.
#[derive(Debug, thiserror::Error)]
pub enum DiffError {
    /// Unknown revision, missing object, or a repository the driver could
    /// not read: retrying will not help.
    #[error("permanent git error: {message}")]
    Permanent { message: String },
    /// A timeout, a transport failure, or any other failure worth retrying.
    #[error("transient git error: {message}")]
    Transient { message: String },
}

/// Blob metadata from a batch lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobMeta {
    pub sha:  String,
    /// `None` when git reports the blob as missing.
    pub size: Option<u64>,
}

/// Enumerate files changed between `base_sha` and `to_sha` via the sandbox.
///
/// Paths from this listing are treated as metadata only; blob reads use
/// the SHAs, not the paths. The `--numstat` companion classifies text vs
/// binary so callers can skip binary contents without ever fetching them.
pub async fn list_changed_files_raw(
    sandbox: &RunSandbox,
    base_sha: &str,
    to_sha: &str,
) -> std::result::Result<Vec<RawDiffEntry>, DiffError> {
    let git = diff_facet(sandbox)?;
    let options = GitDiffOptions::new(GitRevisionRange::new(base_sha).to(to_sha))
        .find_renames(FIND_RENAMES_PERCENT)
        .timeout(RUN_FILES_TIMEOUT);
    let entries = git
        .diff_entries(sandbox.working_directory(), &options)
        .await
        .map_err(|error| diff_error(&error))?;
    entries
        .into_iter()
        .map(raw_diff_entry)
        .collect::<std::result::Result<Vec<_>, String>>()
        .map_err(|message| DiffError::Permanent { message })
}

fn diff_facet(sandbox: &RunSandbox) -> std::result::Result<GitFacet<'_>, DiffError> {
    sandbox.git().map_err(|error| DiffError::Permanent {
        message: fabro_sandbox::display_for_log(&error),
    })
}

/// What a driver failure means for the Run Files endpoint: an unknown
/// revision or missing object is permanent (the handler falls through to
/// the stored patch), and so is output the driver could not read, since a
/// retry reads the same object; a timeout, a transport failure, or anything
/// else is transient and surfaces as a 503 for the client to retry.
fn diff_error(error: &sandbox_driver::Error) -> DiffError {
    let message = fabro_sandbox::display_for_log(error);
    match error {
        sandbox_driver::Error::Io { .. } => DiffError::Permanent { message },
        sandbox_driver::Error::Git(failure) => {
            let stderr = failure
                .output()
                .map(|output| String::from_utf8_lossy(output.stderr()).into_owned())
                .unwrap_or_default();
            if failure.kind() == GitFailureKind::RefNotFound || is_permanent_git_error(&stderr) {
                DiffError::Permanent { message }
            } else {
                DiffError::Transient { message }
            }
        }
        _ => DiffError::Transient { message },
    }
}

fn is_permanent_git_error(stderr: &str) -> bool {
    // git emits these to stderr for unknown revisions / missing objects;
    // treat them as Permanent so the handler falls through to final_patch.
    let lower = stderr.to_lowercase();
    lower.contains("unknown revision")
        || lower.contains("bad revision")
        || lower.contains("bad object")
        || lower.contains("invalid revision")
        || lower.contains("no such path")
        || lower.contains("not a valid object name")
}

/// The Run Files entry for one path of the driver's diff. Mode 120000 is a
/// symlink, 160000 a submodule.
fn raw_diff_entry(entry: GitDiffEntry) -> std::result::Result<RawDiffEntry, String> {
    let is_mode = |mode: &Option<String>, expected: &str| mode.as_deref() == Some(expected);
    let is_symlink = is_mode(&entry.old_mode, "120000") || is_mode(&entry.new_mode, "120000");
    let is_submodule = is_mode(&entry.old_mode, "160000") || is_mode(&entry.new_mode, "160000");
    let path = entry.path;
    let old_blob = entry.old_blob.unwrap_or_default();
    let new_blob = entry.new_blob.unwrap_or_default();
    let old_mode = entry.old_mode.unwrap_or_default();
    let new_mode = entry.new_mode.unwrap_or_default();

    Ok(match (entry.change, is_symlink, is_submodule) {
        (GitChange::Renamed | GitChange::Copied, _, _) => RawDiffEntry::Renamed {
            old_path: entry.old_path.unwrap_or_default(),
            new_path: path,
            old_blob,
            new_blob,
            new_mode,
            similarity: entry.similarity.unwrap_or(0),
        },
        (GitChange::Added, true, _) => RawDiffEntry::Symlink {
            path,
            change_kind: SymlinkChange::Added,
            old_blob: None,
            new_blob: Some(new_blob),
        },
        (GitChange::Added, _, true) => RawDiffEntry::Submodule {
            path,
            change_kind: SubmoduleChange::Added,
        },
        (GitChange::Added, _, _) => RawDiffEntry::Added {
            path,
            new_blob,
            new_mode,
        },
        (GitChange::Deleted, true, _) => RawDiffEntry::Symlink {
            path,
            change_kind: SymlinkChange::Deleted,
            old_blob: Some(old_blob),
            new_blob: None,
        },
        (GitChange::Deleted, _, true) => RawDiffEntry::Submodule {
            path,
            change_kind: SubmoduleChange::Deleted,
        },
        (GitChange::Deleted, _, _) => RawDiffEntry::Deleted {
            path,
            old_blob,
            old_mode,
        },
        (GitChange::Modified | GitChange::TypeChanged, true, _) => RawDiffEntry::Symlink {
            path,
            change_kind: SymlinkChange::Modified,
            old_blob: Some(old_blob),
            new_blob: Some(new_blob),
        },
        (GitChange::Modified | GitChange::TypeChanged, _, true) => RawDiffEntry::Submodule {
            path,
            change_kind: SubmoduleChange::Modified,
        },
        (GitChange::Modified | GitChange::TypeChanged, _, _) => RawDiffEntry::Modified {
            path,
            old_blob,
            new_blob,
            new_mode,
        },
        (other, _, _) => {
            return Err(format!("unknown diff status {other:?} for {path:?}"));
        }
    })
}

pub use fabro_types::{DiffStats, DiffSummary};

/// What `git diff --numstat` says about a range: which paths are binary,
/// plus per-path `+/-` line totals for text files.
#[derive(Debug, Default)]
pub struct DiffNumstat {
    /// Repo-relative paths (post-rename) that git classifies as binary.
    pub binary_paths:       HashSet<String>,
    /// Repo-relative paths (post-rename) to line stats for text files.
    pub line_stats_by_path: HashMap<String, DiffStats>,
}

pub fn summarize_diff_numstat(numstat: &DiffNumstat) -> DiffSummary {
    let text_files = i64::try_from(numstat.line_stats_by_path.len()).unwrap_or(i64::MAX);
    let binary_files = i64::try_from(numstat.binary_paths.len()).unwrap_or(i64::MAX);
    let (additions, deletions) =
        numstat
            .line_stats_by_path
            .values()
            .fold((0_i64, 0_i64), |(adds, dels), stats| {
                (
                    adds.saturating_add(stats.additions),
                    dels.saturating_add(stats.deletions),
                )
            });

    DiffSummary {
        files_changed: text_files.saturating_add(binary_files),
        additions,
        deletions,
    }
}

/// The numstat of `base_sha..to_sha`: the set of binary paths and the
/// text-file `+/-` totals, from one driver call.
pub async fn list_diff_numstat(
    sandbox: &RunSandbox,
    base_sha: &str,
    to_sha: &str,
) -> std::result::Result<DiffNumstat, DiffError> {
    let git = diff_facet(sandbox)?;
    let options = GitDiffOptions::new(GitRevisionRange::new(base_sha).to(to_sha))
        .find_renames(FIND_RENAMES_PERCENT)
        .timeout(RUN_FILES_TIMEOUT);
    let rows = git
        .diff_numstat(sandbox.working_directory(), &options)
        .await
        .map_err(|error| diff_error(&error))?;

    let mut out = DiffNumstat::default();
    for row in rows {
        match (row.additions, row.deletions) {
            (Some(additions), Some(deletions)) => {
                out.line_stats_by_path.insert(row.path, DiffStats {
                    additions: i64::try_from(additions).unwrap_or(i64::MAX),
                    deletions: i64::try_from(deletions).unwrap_or(i64::MAX),
                });
            }
            _ => {
                out.binary_paths.insert(row.path);
            }
        }
    }
    Ok(out)
}

/// Blob sizes for many SHAs in one driver call, in the order of `shas`.
/// A blob git does not have yields `BlobMeta { size: None, .. }`.
pub async fn stream_blob_metadata(
    sandbox: &RunSandbox,
    shas: &[String],
) -> std::result::Result<Vec<BlobMeta>, DiffError> {
    if shas.is_empty() {
        return Ok(Vec::new());
    }
    let git = diff_facet(sandbox)?;
    let sizes = git
        .blob_sizes(sandbox.working_directory(), shas)
        .await
        .map_err(|error| diff_error(&error))?;
    Ok(shas
        .iter()
        .zip(sizes)
        .map(|(sha, size)| BlobMeta {
            sha: sha.clone(),
            size,
        })
        .collect())
}

/// Blob contents for many SHAs in one driver call, in the order of `shas`.
///
/// Contents are size-capped per blob: any blob exceeding `size_cap_bytes`
/// returns `None` in its slot (the caller flags that entry as truncated),
/// as does a blob git does not have or one that is not UTF-8. Callers are
/// expected to have pre-filtered binary blobs via [`list_diff_numstat`].
pub async fn stream_blobs(
    sandbox: &RunSandbox,
    shas: &[String],
    size_cap_bytes: u64,
) -> std::result::Result<Vec<Option<String>>, DiffError> {
    if shas.is_empty() {
        return Ok(Vec::new());
    }
    let git = diff_facet(sandbox)?;
    let blobs = git
        .blobs(sandbox.working_directory(), shas, size_cap_bytes)
        .await
        .map_err(|error| diff_error(&error))?;
    Ok(blobs
        .into_iter()
        .map(|blob| blob.and_then(|bytes| String::from_utf8(bytes).ok()))
        .collect())
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::disallowed_methods,
        reason = "These unit tests use the real git CLI to construct sandbox-git fixture repositories and sync-write fixtures to disk."
    )]

    use super::*;

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

        let sandbox = fabro_sandbox::local_sandbox(repo.to_path_buf())
            .await
            .unwrap();
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

        let sandbox = fabro_sandbox::local_sandbox(repo.to_path_buf())
            .await
            .unwrap();
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
    async fn list_diff_numstat_flags_png_and_aggregates_text_lines() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        init_git_repo(repo);

        std::fs::write(repo.join("doc.md"), "hi\nthere\n").unwrap();
        let base = git_commit_all(repo, "initial");

        // doc.md: replace 2 lines with 3 lines → adds=3, dels=2
        std::fs::write(repo.join("doc.md"), "alpha\nbeta\ngamma\n").unwrap();
        // Minimal PNG header (8-byte signature) + a chunk — git classifies
        // this as binary via NUL-byte detection.
        let png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H',
            b'D', b'R',
        ];
        std::fs::write(repo.join("logo.png"), png).unwrap();
        let head = git_commit_all(repo, "change");

        let sandbox = fabro_sandbox::local_sandbox(repo.to_path_buf())
            .await
            .unwrap();
        let stats = list_diff_numstat(&sandbox, &base, &head).await.unwrap();

        assert!(
            stats.binary_paths.contains("logo.png"),
            "binary_paths: {:?}",
            stats.binary_paths
        );
        assert!(
            !stats.binary_paths.contains("doc.md"),
            "binary_paths: {:?}",
            stats.binary_paths
        );
        let doc_stats = stats.line_stats_by_path.get("doc.md").unwrap();
        assert_eq!(doc_stats.additions, 3, "additions: {stats:?}");
        assert_eq!(doc_stats.deletions, 2, "deletions: {stats:?}");
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

        let sandbox = fabro_sandbox::local_sandbox(repo.to_path_buf())
            .await
            .unwrap();
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

        let sandbox = fabro_sandbox::local_sandbox(repo.to_path_buf())
            .await
            .unwrap();
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

        let sandbox = fabro_sandbox::local_sandbox(repo.to_path_buf())
            .await
            .unwrap();
        let err =
            list_changed_files_raw(&sandbox, "0000000000000000000000000000000000000000", "HEAD")
                .await
                .expect_err("expected error for unknown base sha");
        assert!(matches!(err, DiffError::Permanent { .. }), "err: {err:?}");
    }
}
