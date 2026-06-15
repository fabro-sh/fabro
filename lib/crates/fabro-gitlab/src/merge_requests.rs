use fabro_types::pull_request::{
    PullRequestDetails, PullRequestLink, PullRequestTimestamps, PullRequestUser,
};
use fabro_types::settings::run::MergeStrategy;
use serde::{Deserialize, Serialize};

use crate::repository::{GitLabBaseUrl, GitLabRepository, encode_path_segment};
use crate::{GitLabContext, GitLabCredentials, GitLabError, Result};

#[derive(Debug, Serialize)]
pub struct CreateMergeRequestRequest<'a> {
    pub source_branch:        &'a str,
    pub target_branch:        &'a str,
    pub title:                &'a str,
    pub description:          &'a str,
    pub draft:                bool,
    pub remove_source_branch: bool,
}

#[derive(Debug, Deserialize)]
pub struct GitLabMergeRequest {
    pub iid:                   u64,
    pub title:                 String,
    pub description:           Option<String>,
    pub web_url:               String,
    pub state:                 String,
    #[serde(default)]
    pub draft:                 bool,
    #[serde(default)]
    pub work_in_progress:      bool,
    #[serde(default)]
    pub merged_at:             Option<String>,
    pub merge_status:          Option<String>,
    pub detailed_merge_status: Option<String>,
    #[serde(default)]
    pub changes_count:         Option<String>,
    pub source_branch:         String,
    pub target_branch:         String,
    pub author:                Option<GitLabMergeRequestAuthor>,
    pub created_at:            String,
    pub updated_at:            String,
}

#[derive(Debug, Deserialize)]
pub struct GitLabMergeRequestAuthor {
    pub username: String,
}

#[derive(Debug, Serialize)]
pub struct CloseMergeRequestRequest {
    pub state_event: &'static str,
}

impl CloseMergeRequestRequest {
    pub const CLOSE: Self = Self {
        state_event: "close",
    };
}

#[must_use]
pub fn merge_requests_url(base: &GitLabBaseUrl, repo: &GitLabRepository) -> String {
    base.api_url(&format!(
        "projects/{}/merge_requests",
        repo.encoded_project_id
    ))
    .to_string()
}

#[must_use]
pub fn merge_request_url(base: &GitLabBaseUrl, repo: &GitLabRepository, iid: u64) -> String {
    base.api_url(&format!(
        "projects/{}/merge_requests/{iid}",
        repo.encoded_project_id
    ))
    .to_string()
}

#[must_use]
pub fn merge_url(base: &GitLabBaseUrl, repo: &GitLabRepository, iid: u64) -> String {
    base.api_url(&format!(
        "projects/{}/merge_requests/{iid}/merge",
        repo.encoded_project_id
    ))
    .to_string()
}

#[must_use]
pub fn branch_url(base: &GitLabBaseUrl, repo: &GitLabRepository, branch: &str) -> String {
    let branch = encode_path_segment(branch);
    base.api_url(&format!(
        "projects/{}/repository/branches/{branch}",
        repo.encoded_project_id
    ))
    .to_string()
}

fn http_client(ctx: &GitLabContext) -> Result<fabro_http::HttpClient> {
    ctx.http_client.clone().map_or_else(
        || fabro_http::http_client().map_err(|err| GitLabError::Api(err.into())),
        Ok,
    )
}

fn base_url(ctx: &GitLabContext) -> Result<GitLabBaseUrl> {
    GitLabBaseUrl::parse(ctx.base_url.as_str())
}

fn token(ctx: &GitLabContext) -> &str {
    match &ctx.credentials {
        GitLabCredentials::Token(token) => token,
    }
}

async fn send(
    request: fabro_http::RequestBuilder,
    operation: &'static str,
) -> Result<fabro_http::Response> {
    request
        .send()
        .await
        .map_err(|err| GitLabError::Api(anyhow::Error::new(err).context(operation)))
}

async fn response_text(resp: fabro_http::Response) -> String {
    resp.text().await.unwrap_or_default()
}

#[allow(
    clippy::too_many_arguments,
    reason = "Creating a merge request needs explicit repo, branch, and body fields."
)]
pub async fn create_merge_request(
    ctx: &GitLabContext,
    repo: &GitLabRepository,
    target_branch: &str,
    source_branch: &str,
    title: &str,
    description: &str,
    draft: bool,
) -> Result<GitLabMergeRequest> {
    let base = base_url(ctx)?;
    let client = http_client(ctx)?;
    let body = CreateMergeRequestRequest {
        source_branch,
        target_branch,
        title,
        description,
        draft,
        remove_source_branch: false,
    };
    let resp = send(
        client
            .post(merge_requests_url(&base, repo))
            .header("PRIVATE-TOKEN", token(ctx))
            .json(&body),
        "Failed to create GitLab merge request",
    )
    .await?;
    let status = resp.status();
    if status != fabro_http::StatusCode::CREATED {
        let body = response_text(resp).await;
        return Err(GitLabError::Api(anyhow::anyhow!(
            "Unexpected status {status} creating GitLab merge request: {body}"
        )));
    }

    resp.json()
        .await
        .map_err(|err| GitLabError::Api(anyhow::Error::new(err)))
}

pub async fn get_merge_request(
    ctx: &GitLabContext,
    repo: &GitLabRepository,
    iid: u64,
) -> Result<GitLabMergeRequest> {
    let base = base_url(ctx)?;
    let client = http_client(ctx)?;
    let resp = send(
        client
            .get(merge_request_url(&base, repo, iid))
            .header("PRIVATE-TOKEN", token(ctx)),
        "Failed to fetch GitLab merge request",
    )
    .await?;
    let status = resp.status();
    if status == fabro_http::StatusCode::NOT_FOUND {
        return Err(GitLabError::NotFound {
            resource: "merge request",
            iid,
        });
    }
    if !status.is_success() {
        let body = response_text(resp).await;
        return Err(GitLabError::Api(anyhow::anyhow!(
            "Unexpected status {status} fetching GitLab merge request: {body}"
        )));
    }

    resp.json()
        .await
        .map_err(|err| GitLabError::Api(anyhow::Error::new(err)))
}

pub async fn merge_merge_request(
    ctx: &GitLabContext,
    repo: &GitLabRepository,
    iid: u64,
    method: MergeStrategy,
) -> Result<()> {
    let base = base_url(ctx)?;
    let client = http_client(ctx)?;
    let squash = matches!(method, MergeStrategy::Squash);
    let resp = send(
        client
            .put(merge_url(&base, repo, iid))
            .header("PRIVATE-TOKEN", token(ctx))
            .json(&serde_json::json!({ "squash": squash })),
        "Failed to merge GitLab merge request",
    )
    .await?;
    let status = resp.status();
    if status == fabro_http::StatusCode::NOT_FOUND {
        return Err(GitLabError::NotFound {
            resource: "merge request",
            iid,
        });
    }
    if !status.is_success() {
        let body = response_text(resp).await;
        return Err(GitLabError::Api(anyhow::anyhow!(
            "Unexpected status {status} merging GitLab merge request: {body}"
        )));
    }
    Ok(())
}

pub async fn close_merge_request(
    ctx: &GitLabContext,
    repo: &GitLabRepository,
    iid: u64,
) -> Result<()> {
    let base = base_url(ctx)?;
    let client = http_client(ctx)?;
    let resp = send(
        client
            .put(merge_request_url(&base, repo, iid))
            .header("PRIVATE-TOKEN", token(ctx))
            .json(&CloseMergeRequestRequest::CLOSE),
        "Failed to close GitLab merge request",
    )
    .await?;
    let status = resp.status();
    if status == fabro_http::StatusCode::NOT_FOUND {
        return Err(GitLabError::NotFound {
            resource: "merge request",
            iid,
        });
    }
    if !status.is_success() {
        let body = response_text(resp).await;
        return Err(GitLabError::Api(anyhow::anyhow!(
            "Unexpected status {status} closing GitLab merge request: {body}"
        )));
    }
    Ok(())
}

pub async fn enable_auto_merge(
    ctx: &GitLabContext,
    repo: &GitLabRepository,
    iid: u64,
    method: MergeStrategy,
) -> Result<()> {
    let base = base_url(ctx)?;
    let client = http_client(ctx)?;
    let squash = matches!(method, MergeStrategy::Squash);
    let resp = send(
        client
            .put(merge_url(&base, repo, iid))
            .header("PRIVATE-TOKEN", token(ctx))
            .json(&serde_json::json!({
                "auto_merge": true,
                "merge_when_pipeline_succeeds": true,
                "squash": squash,
            })),
        "Failed to enable GitLab auto-merge",
    )
    .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = response_text(resp).await;
        return Err(GitLabError::Api(anyhow::anyhow!(
            "Unexpected status {status} enabling GitLab auto-merge: {body}"
        )));
    }
    Ok(())
}

#[must_use]
pub fn to_pull_request_link(repo: &GitLabRepository, mr: &GitLabMergeRequest) -> PullRequestLink {
    PullRequestLink::gitlab(&repo.owner_path, &repo.repo, mr.iid, &mr.web_url)
}

#[must_use]
pub fn to_pull_request_details(
    _repo: &GitLabRepository,
    mr: GitLabMergeRequest,
) -> PullRequestDetails {
    let mergeable = mr
        .detailed_merge_status
        .as_deref()
        .or(mr.merge_status.as_deref())
        .and_then(|status| match status {
            "mergeable" | "can_be_merged" => Some(true),
            "cannot_be_merged" => Some(false),
            _ => None,
        });
    let changed_files = mr
        .changes_count
        .as_deref()
        .and_then(parse_changes_count)
        .unwrap_or(0);
    let merged = mr.state == "merged" || mr.merged_at.is_some();

    PullRequestDetails {
        title: mr.title,
        body: mr.description,
        state: mr.state,
        draft: mr.draft || mr.work_in_progress,
        merged,
        merged_at: mr.merged_at,
        mergeable,
        additions: 0,
        deletions: 0,
        changed_files,
        author: PullRequestUser {
            login: mr
                .author
                .map_or_else(|| "unknown".to_string(), |author| author.username),
        },
        head_branch: mr.source_branch,
        base_branch: mr.target_branch,
        timestamps: PullRequestTimestamps {
            created_at: mr.created_at,
            updated_at: mr.updated_at,
        },
    }
}

fn parse_changes_count(value: &str) -> Option<u64> {
    let digits = value
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::{CreateMergeRequestRequest, parse_changes_count};

    #[test]
    fn parses_numeric_changes_count_prefix() {
        assert_eq!(parse_changes_count("17"), Some(17));
        assert_eq!(parse_changes_count("1000+"), Some(1000));
        assert_eq!(parse_changes_count("many"), None);
    }

    #[test]
    fn create_merge_request_request_serializes_draft() {
        let body = serde_json::to_value(CreateMergeRequestRequest {
            source_branch:        "fabro/run-1",
            target_branch:        "main",
            title:                "Ship it",
            description:          "Body",
            draft:                true,
            remove_source_branch: false,
        })
        .unwrap();

        assert_eq!(body["draft"], true);
    }
}
