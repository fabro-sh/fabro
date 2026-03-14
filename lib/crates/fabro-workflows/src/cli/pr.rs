use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args;
use tracing::info;

use crate::cli::runs::{default_runs_base, resolve_run, scan_runs};
use crate::conclusion::Conclusion;
use crate::manifest::Manifest;
use crate::outcome::StageStatus;
use crate::pull_request::PullRequestRecord;

#[derive(Args)]
pub struct PrCreateArgs {
    /// Run ID or prefix
    pub run_id: String,
    /// LLM model for generating PR description
    #[arg(long)]
    pub model: Option<String>,
}

#[derive(Args)]
pub struct PrListArgs {
    /// Show all PRs (including closed/merged), not just open
    #[arg(long)]
    pub all: bool,
}

#[derive(Args)]
pub struct PrViewArgs {
    /// Run ID or prefix
    pub run_id: String,
}

#[derive(Args)]
pub struct PrMergeArgs {
    /// Run ID or prefix
    pub run_id: String,
    /// Merge method: merge, squash, or rebase
    #[arg(long, default_value = "squash")]
    pub method: String,
}

#[derive(Args)]
pub struct PrCloseArgs {
    /// Run ID or prefix
    pub run_id: String,
}

fn load_pr_record(base: &Path, run_id: &str) -> Result<(PullRequestRecord, PathBuf)> {
    let run_dir = resolve_run(base, run_id)?.path;
    let pr_path = run_dir.join("pull_request.json");
    let content = std::fs::read_to_string(&pr_path).with_context(|| {
        format!(
            "No pull_request.json found in run directory. \
             Create one first with: fabro pr create {run_id}"
        )
    })?;
    let record: PullRequestRecord =
        serde_json::from_str(&content).context("Failed to parse pull_request.json")?;
    Ok((record, run_dir))
}

pub async fn pr_list_command(
    args: PrListArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let base = default_runs_base();
    pr_list_from(&base, args, github_app).await
}

async fn pr_list_from(
    base: &Path,
    args: PrListArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    let runs = scan_runs(base).context("Failed to scan runs")?;

    let mut entries: Vec<(String, PullRequestRecord)> = Vec::new();
    for run in &runs {
        let pr_path = run.path.join("pull_request.json");
        if let Ok(content) = std::fs::read_to_string(&pr_path) {
            if let Ok(record) = serde_json::from_str::<PullRequestRecord>(&content) {
                entries.push((run.run_id.clone(), record));
            }
        }
    }

    if entries.is_empty() {
        println!("No pull requests found.");
        return Ok(());
    }

    struct PrRow {
        run_id: String,
        number: u64,
        state: String,
        title: String,
        url: String,
    }

    // Fetch live state for all PRs concurrently
    let futures: Vec<_> = entries
        .iter()
        .map(|(run_id, record)| {
            let creds = creds.clone();
            let run_id = run_id.clone();
            let record = record.clone();
            async move {
                match fabro_github::get_pull_request(
                    &creds,
                    &record.owner,
                    &record.repo,
                    record.number,
                    fabro_github::GITHUB_API_BASE_URL,
                )
                .await
                {
                    Ok(detail) => PrRow {
                        run_id,
                        number: detail.number,
                        state: if detail.draft {
                            "draft".to_string()
                        } else {
                            detail.state
                        },
                        title: detail.title,
                        url: detail.html_url,
                    },
                    Err(e) => {
                        tracing::warn!(run_id, error = %e, "Failed to fetch PR state");
                        PrRow {
                            run_id,
                            number: record.number,
                            state: "unknown".to_string(),
                            title: record.title,
                            url: record.html_url,
                        }
                    }
                }
            }
        })
        .collect();

    let all_rows = futures::future::join_all(futures).await;
    let rows: Vec<_> = if args.all {
        all_rows
    } else {
        all_rows
            .into_iter()
            .filter(|r| r.state == "open" || r.state == "draft" || r.state == "unknown")
            .collect()
    };

    if rows.is_empty() {
        println!("No open pull requests found. Use --all to include closed/merged.");
        return Ok(());
    }

    // Print table
    println!(
        "{:<12} {:<6} {:<8} {:<50} URL",
        "RUN", "#", "STATE", "TITLE"
    );
    for row in &rows {
        let short_id = if row.run_id.len() > 12 {
            &row.run_id[..12]
        } else {
            &row.run_id
        };
        let short_title = if row.title.len() > 50 {
            format!("{}…", &row.title[..row.title.floor_char_boundary(49)])
        } else {
            row.title.clone()
        };
        println!(
            "{:<12} {:<6} {:<8} {:<50} {}",
            short_id, row.number, row.state, short_title, row.url
        );
    }

    info!(count = rows.len(), "Listed pull requests");
    Ok(())
}

pub async fn pr_view_command(
    args: PrViewArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let base = default_runs_base();
    pr_view_from(&base, args, github_app).await
}

async fn pr_view_from(
    base: &Path,
    args: PrViewArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let (record, _run_dir) = load_pr_record(base, &args.run_id)?;

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    let detail = fabro_github::get_pull_request(
        &creds,
        &record.owner,
        &record.repo,
        record.number,
        fabro_github::GITHUB_API_BASE_URL,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    info!(number = detail.number, owner = %record.owner, repo = %record.repo, "Viewing pull request");

    println!("#{} {}", detail.number, detail.title);
    let state_display = if detail.draft { "draft" } else { &detail.state };
    println!("State:   {state_display}");
    println!("URL:     {}", detail.html_url);
    println!(
        "Branch:  {} → {}",
        detail.head.ref_name, detail.base.ref_name
    );
    println!("Author:  {}", detail.user.login);
    println!(
        "Changes: +{} -{} ({} files)",
        detail.additions, detail.deletions, detail.changed_files
    );
    if let Some(body) = &detail.body {
        if !body.is_empty() {
            println!();
            println!("{body}");
        }
    }

    Ok(())
}

pub async fn pr_merge_command(
    args: PrMergeArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let base = default_runs_base();
    pr_merge_from(&base, args, github_app).await
}

async fn pr_merge_from(
    base: &Path,
    args: PrMergeArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let (record, _run_dir) = load_pr_record(base, &args.run_id)?;

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    fabro_github::merge_pull_request(
        &creds,
        &record.owner,
        &record.repo,
        record.number,
        &args.method,
        fabro_github::GITHUB_API_BASE_URL,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    info!(number = record.number, owner = %record.owner, repo = %record.repo, method = %args.method, "Merged pull request");
    println!("Merged #{} ({})", record.number, record.html_url);

    Ok(())
}

pub async fn pr_close_command(
    args: PrCloseArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let base = default_runs_base();
    pr_close_from(&base, args, github_app).await
}

async fn pr_close_from(
    base: &Path,
    args: PrCloseArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let (record, _run_dir) = load_pr_record(base, &args.run_id)?;

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    fabro_github::close_pull_request(
        &creds,
        &record.owner,
        &record.repo,
        record.number,
        fabro_github::GITHUB_API_BASE_URL,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    info!(number = record.number, owner = %record.owner, repo = %record.repo, "Closed pull request");
    println!("Closed #{} ({})", record.number, record.html_url);

    Ok(())
}

pub async fn pr_create_command(
    args: PrCreateArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let base = default_runs_base();
    pr_create_from(&base, args, github_app).await
}

async fn pr_create_from(
    base: &Path,
    args: PrCreateArgs,
    github_app: Option<fabro_github::GitHubAppCredentials>,
) -> Result<()> {
    let run_dir = resolve_run(base, &args.run_id)?.path;

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

    let https_url = fabro_github::ssh_url_to_https(&origin_url);
    let (owner, repo) =
        fabro_github::parse_github_owner_repo(&https_url).map_err(|e| anyhow::anyhow!("{e}"))?;

    let creds = github_app.context(
        "GitHub App credentials required — set GITHUB_APP_PRIVATE_KEY and configure app_id",
    )?;

    let branch_found = fabro_github::branch_exists(
        &creds,
        &owner,
        &repo,
        run_branch,
        fabro_github::GITHUB_API_BASE_URL,
    )
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
        .unwrap_or_else(|| fabro_llm::catalog::default_model().id.to_string());

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

    fn make_pr_record(base: &Path, run_id: &str) {
        let dir_name = format!("20260101-{}", &run_id[..6].to_uppercase());
        let run_dir = base.join(&dir_name);
        fs::create_dir_all(&run_dir).unwrap();

        // Write minimal manifest.json so scan_runs finds it
        let manifest = serde_json::json!({
            "run_id": run_id,
            "workflow_name": "test",
            "goal": "fix bug",
            "start_time": "2026-01-01T12:00:00Z",
            "node_count": 1,
            "edge_count": 0
        });
        fs::write(
            run_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let record = PullRequestRecord {
            html_url: format!("https://github.com/owner/repo/pull/42"),
            number: 42,
            owner: "owner".to_string(),
            repo: "repo".to_string(),
            base_branch: "main".to_string(),
            head_branch: "fabro/run/abc".to_string(),
            title: "Fix the thing".to_string(),
        };
        fs::write(
            run_dir.join("pull_request.json"),
            serde_json::to_string_pretty(&record).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn load_pr_record_success() {
        let tmp = tempfile::tempdir().unwrap();
        make_pr_record(tmp.path(), "abc123-test");

        let (record, _dir) = load_pr_record(tmp.path(), "abc123").unwrap();
        assert_eq!(record.number, 42);
        assert_eq!(record.owner, "owner");
        assert_eq!(record.repo, "repo");
    }

    #[test]
    fn load_pr_record_missing() {
        let tmp = tempfile::tempdir().unwrap();

        // Create run dir without pull_request.json
        let dir_name = "20260101-ABC123";
        let run_dir = tmp.path().join(dir_name);
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(
            run_dir.join("manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "run_id": "abc123-test",
                "workflow_name": "test",
                "goal": "fix bug",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            }))
            .unwrap(),
        )
        .unwrap();

        let err = load_pr_record(tmp.path(), "abc123").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("pull_request.json"), "got: {msg}");
        assert!(msg.contains("fabro pr create"), "got: {msg}");
    }

    #[test]
    fn pr_list_finds_runs_with_prs() {
        let tmp = tempfile::tempdir().unwrap();

        // Run 1: has PR
        make_pr_record(tmp.path(), "aaa111-test");

        // Run 2: no PR
        let dir2 = tmp.path().join("20260101-BBB222");
        fs::create_dir_all(&dir2).unwrap();
        fs::write(
            dir2.join("manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "run_id": "bbb222-test",
                "workflow_name": "test",
                "goal": "another task",
                "start_time": "2026-01-01T13:00:00Z",
                "node_count": 1,
                "edge_count": 0
            }))
            .unwrap(),
        )
        .unwrap();

        // Run 3: has PR
        let dir3 = tmp.path().join("20260101-CCC333");
        fs::create_dir_all(&dir3).unwrap();
        fs::write(
            dir3.join("manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "run_id": "ccc333-test",
                "workflow_name": "test",
                "goal": "third task",
                "start_time": "2026-01-01T14:00:00Z",
                "node_count": 1,
                "edge_count": 0
            }))
            .unwrap(),
        )
        .unwrap();
        let record3 = PullRequestRecord {
            html_url: "https://github.com/owner/repo/pull/99".to_string(),
            number: 99,
            owner: "owner".to_string(),
            repo: "repo".to_string(),
            base_branch: "main".to_string(),
            head_branch: "fabro/run/ccc".to_string(),
            title: "Another fix".to_string(),
        };
        fs::write(
            dir3.join("pull_request.json"),
            serde_json::to_string_pretty(&record3).unwrap(),
        )
        .unwrap();

        // Verify scan finds the right runs with PRs
        let runs = scan_runs(tmp.path()).unwrap();
        let runs_with_prs: Vec<_> = runs
            .iter()
            .filter(|r| r.path.join("pull_request.json").exists())
            .collect();
        assert_eq!(runs_with_prs.len(), 2);
    }

    #[tokio::test]
    async fn pr_view_fails_no_pr_record() {
        let tmp = tempfile::tempdir().unwrap();

        let dir = tmp.path().join("20260101-ABC123");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "run_id": "abc123-test",
                "workflow_name": "test",
                "goal": "fix bug",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            }))
            .unwrap(),
        )
        .unwrap();

        let args = PrViewArgs {
            run_id: "abc123".to_string(),
        };
        let result = pr_view_from(tmp.path(), args, None).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("pull_request.json"), "got: {err}");
    }

    #[tokio::test]
    async fn pr_merge_fails_no_pr_record() {
        let tmp = tempfile::tempdir().unwrap();

        let dir = tmp.path().join("20260101-ABC123");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "run_id": "abc123-test",
                "workflow_name": "test",
                "goal": "fix bug",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            }))
            .unwrap(),
        )
        .unwrap();

        let args = PrMergeArgs {
            run_id: "abc123".to_string(),
            method: "squash".to_string(),
        };
        let result = pr_merge_from(tmp.path(), args, None).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("pull_request.json"), "got: {err}");
    }

    #[tokio::test]
    async fn pr_close_fails_no_pr_record() {
        let tmp = tempfile::tempdir().unwrap();

        let dir = tmp.path().join("20260101-ABC123");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "run_id": "abc123-test",
                "workflow_name": "test",
                "goal": "fix bug",
                "start_time": "2026-01-01T12:00:00Z",
                "node_count": 1,
                "edge_count": 0
            }))
            .unwrap(),
        )
        .unwrap();

        let args = PrCloseArgs {
            run_id: "abc123".to_string(),
        };
        let result = pr_close_from(tmp.path(), args, None).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("pull_request.json"), "got: {err}");
    }

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
                "run_branch": "fabro/run/abc123"
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
                "run_branch": "fabro/run/abc123"
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
                "run_branch": "fabro/run/abc123"
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
                "run_branch": "fabro/run/abc123"
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
                "run_branch": "fabro/run/abc123"
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
