use std::path::Path;

use fabro_agent::Sandbox;
use fabro_checkpoint::trailer as trailerlink;
use fabro_checkpoint::trailer::Trailer;
use fabro_types::RunId;

use crate::asset_snapshot;
use crate::git::{GitAuthor, blocking_push_with_timeout, push_ref};
use fabro_sandbox::daytona::detect_repo_info;

/// Captured git state for a workflow run, shared with handlers.
#[derive(Debug, Clone)]
pub struct GitState {
    pub run_id: RunId,
    pub base_sha: String,
    pub run_branch: Option<String>,
    pub meta_branch: Option<String>,
    pub checkpoint_exclude_globs: Vec<String>,
    pub git_author: GitAuthor,
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
    let mut all_excludes: Vec<String> = asset_snapshot::EXCLUDE_DIRS
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
            key: "Fabro-Run",
            value: run_id,
        },
        Trailer {
            key: "Fabro-Completed",
            value: &completed_str,
        },
    ];
    let shadow_sha_ref = shadow_sha.as_deref().unwrap_or("");
    if shadow_sha.is_some() {
        trailers.push(Trailer {
            key: "Fabro-Checkpoint",
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
/// Authenticates via a GitHub App installation token so we don't depend
/// on the host's ambient git credentials.
pub async fn git_push_host(
    repo_path: &Path,
    refspec: &str,
    github_app: &Option<fabro_github::GitHubAppCredentials>,
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
        match fabro_github::resolve_authenticated_url(creds, &https_url).await {
            Ok(url) => url,
            Err(e) => {
                tracing::warn!(error = %e, label, "Failed to get token for push");
                return false;
            }
        }
    } else {
        tracing::warn!(label, "No GitHub App credentials for push");
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

/// Run a git diff via the sandbox.
pub(crate) async fn git_diff(
    sandbox: &dyn Sandbox,
    base: &str,
) -> std::result::Result<String, String> {
    let cmd = format!("{GIT_REMOTE} diff {base} HEAD");
    match sandbox.exec_command(&cmd, 30_000, None, None, None).await {
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

#[cfg(test)]
mod tests {
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

        // Call git_checkpoint with empty user excludes — built-in excludes should still apply
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
}
