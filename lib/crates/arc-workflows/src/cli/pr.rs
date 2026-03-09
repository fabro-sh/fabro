use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::Args;
use tracing::info;

use crate::cli::runs::{default_logs_base, find_run_by_prefix};
use crate::conclusion::Conclusion;
use crate::manifest::Manifest;
use crate::outcome::StageStatus;

#[derive(Args)]
pub struct PrCreateArgs {
    /// Run ID or prefix
    pub run_id: String,
    /// LLM model for generating PR description
    #[arg(long)]
    pub model: Option<String>,
}

pub async fn pr_create_command(
    args: PrCreateArgs,
    github_app: Option<arc_github::GitHubAppCredentials>,
) -> Result<()> {
    let base = default_logs_base();
    pr_create_from(&base, args, github_app).await
}

async fn pr_create_from(
    base: &Path,
    args: PrCreateArgs,
    github_app: Option<arc_github::GitHubAppCredentials>,
) -> Result<()> {
    let run_dir = find_run_by_prefix(base, &args.run_id)?;

    let manifest =
        Manifest::load(&run_dir.join("manifest.json")).context("Failed to load manifest.json")?;

    let conclusion = Conclusion::load(&run_dir.join("conclusion.json"))
        .context("Failed to load conclusion.json — is the run finished?")?;

    match conclusion.status {
        StageStatus::Success | StageStatus::PartialSuccess => {}
        status => bail!("Run status is '{status}', expected success or partial_success"),
    }

    let run_branch = manifest
        .run_branch
        .as_deref()
        .context("Run has no run_branch — was it run with git push enabled?")?;

    let diff = std::fs::read_to_string(run_dir.join("final.patch"))
        .context("Failed to read final.patch — no diff available")?;
    if diff.trim().is_empty() {
        bail!("final.patch is empty — nothing to create a PR for");
    }

    let cwd = std::env::current_dir().context("Failed to get current directory")?;
    let (origin_url, detected_branch) =
        crate::daytona_sandbox::detect_repo_info(&cwd).map_err(|e| anyhow::anyhow!("{e}"))?;

    let base_branch = manifest
        .base_branch
        .as_deref()
        .or(detected_branch.as_deref())
        .unwrap_or("main");

    let https_url = arc_github::ssh_url_to_https(&origin_url);
    let (owner, repo) =
        arc_github::parse_github_owner_repo(&https_url).map_err(|e| anyhow::anyhow!("{e}"))?;

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    let branch_found =
        arc_github::branch_exists(&creds, &owner, &repo, run_branch, "https://api.github.com")
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;

    if !branch_found {
        bail!(
            "Branch '{run_branch}' not found on GitHub. \
             Was it pushed? Try: git push origin {run_branch}"
        );
    }

    let model = args
        .model
        .unwrap_or_else(|| arc_llm::catalog::default_model().id.to_string());

    let record = crate::pull_request::maybe_open_pull_request(
        &creds,
        &origin_url,
        base_branch,
        run_branch,
        &manifest.goal,
        &diff,
        &model,
        true,
        &run_dir,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    match record {
        Some(record) => {
            info!(pr_url = %record.html_url, "Pull request created");
            if let Err(e) = record.save(&run_dir.join("pull_request.json")) {
                tracing::warn!(error = %e, "Failed to save pull_request.json");
            }
            println!("{}", record.html_url);
        }
        None => {
            println!("No pull request created (empty diff).");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_test_run(
        base: &Path,
        manifest_json: serde_json::Value,
        conclusion_json: Option<serde_json::Value>,
        diff: Option<&str>,
    ) -> String {
        let run_id = manifest_json["run_id"].as_str().unwrap();
        let dir_name = format!("20260101-{}", &run_id[..6].to_uppercase());
        let run_dir = base.join(&dir_name);
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(
            run_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest_json).unwrap(),
        )
        .unwrap();
        if let Some(c) = conclusion_json {
            fs::write(
                run_dir.join("conclusion.json"),
                serde_json::to_string_pretty(&c).unwrap(),
            )
            .unwrap();
        }
        if let Some(d) = diff {
            fs::write(run_dir.join("final.patch"), d).unwrap();
        }
        run_id.to_string()
    }

    #[tokio::test]
    async fn pr_create_fails_missing_conclusion() {
        let tmp = tempfile::tempdir().unwrap();
        let run_id = make_test_run(
            tmp.path(),
            serde_json::json!({
                "run_id": "abc123-test",
                "workflow_name": "test",
                "goal": "fix bug",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0,
                "run_branch": "arc/run/abc123"
            }),
            None,
            Some("diff content"),
        );

        let args = PrCreateArgs {
            run_id,
            model: None,
        };
        let result = pr_create_from(tmp.path(), args, None).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("conclusion"), "got: {err}");
    }

    #[tokio::test]
    async fn pr_create_fails_on_failed_run() {
        let tmp = tempfile::tempdir().unwrap();
        let run_id = make_test_run(
            tmp.path(),
            serde_json::json!({
                "run_id": "abc123-test",
                "workflow_name": "test",
                "goal": "fix bug",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0,
                "run_branch": "arc/run/abc123"
            }),
            Some(serde_json::json!({
                "timestamp": "2026-01-01T12:01:00Z",
                "status": "fail",
                "duration_ms": 60000
            })),
            Some("diff content"),
        );

        let args = PrCreateArgs {
            run_id,
            model: None,
        };
        let result = pr_create_from(tmp.path(), args, None).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("fail"), "got: {err}");
    }

    #[tokio::test]
    async fn pr_create_fails_missing_run_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let run_id = make_test_run(
            tmp.path(),
            serde_json::json!({
                "run_id": "abc123-test",
                "workflow_name": "test",
                "goal": "fix bug",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            }),
            Some(serde_json::json!({
                "timestamp": "2026-01-01T12:01:00Z",
                "status": "success",
                "duration_ms": 60000
            })),
            Some("diff content"),
        );

        let args = PrCreateArgs {
            run_id,
            model: None,
        };
        let result = pr_create_from(tmp.path(), args, None).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("run_branch"), "got: {err}");
    }

    #[tokio::test]
    async fn pr_create_fails_missing_diff() {
        let tmp = tempfile::tempdir().unwrap();
        let run_id = make_test_run(
            tmp.path(),
            serde_json::json!({
                "run_id": "abc123-test",
                "workflow_name": "test",
                "goal": "fix bug",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0,
                "run_branch": "arc/run/abc123"
            }),
            Some(serde_json::json!({
                "timestamp": "2026-01-01T12:01:00Z",
                "status": "success",
                "duration_ms": 60000
            })),
            None,
        );

        let args = PrCreateArgs {
            run_id,
            model: None,
        };
        let result = pr_create_from(tmp.path(), args, None).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("final.patch"), "got: {err}");
    }

    #[tokio::test]
    async fn pr_create_fails_empty_diff() {
        let tmp = tempfile::tempdir().unwrap();
        let run_id = make_test_run(
            tmp.path(),
            serde_json::json!({
                "run_id": "abc123-test",
                "workflow_name": "test",
                "goal": "fix bug",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0,
                "run_branch": "arc/run/abc123"
            }),
            Some(serde_json::json!({
                "timestamp": "2026-01-01T12:01:00Z",
                "status": "success",
                "duration_ms": 60000
            })),
            Some("  \n  "),
        );

        let args = PrCreateArgs {
            run_id,
            model: None,
        };
        let result = pr_create_from(tmp.path(), args, None).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("empty"), "got: {err}");
    }

    #[tokio::test]
    async fn pr_create_fails_missing_github_creds() {
        let tmp = tempfile::tempdir().unwrap();
        let run_id = make_test_run(
            tmp.path(),
            serde_json::json!({
                "run_id": "abc123-test",
                "workflow_name": "test",
                "goal": "fix bug",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0,
                "run_branch": "arc/run/abc123"
            }),
            Some(serde_json::json!({
                "timestamp": "2026-01-01T12:01:00Z",
                "status": "success",
                "duration_ms": 60000
            })),
            Some("diff content"),
        );

        let args = PrCreateArgs {
            run_id,
            model: None,
        };
        let result = pr_create_from(tmp.path(), args, None).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("GitHub App"), "got: {err}");
    }
}
