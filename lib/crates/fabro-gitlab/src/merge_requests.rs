use fabro_types::pull_request::{
    PullRequestDetails, PullRequestLink, PullRequestTimestamps, PullRequestUser,
};
use fabro_types::settings::run::MergeStrategy;
use serde::{Deserialize, Serialize};

use crate::repository::{GitLabBaseUrl, GitLabRepository};
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

#[derive(Debug, Deserialize)]
struct GitLabProject {
    id:                  u64,
    path_with_namespace: String,
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

fn merge_requests_url_for_project(base: &GitLabBaseUrl, project_id: &str) -> String {
    base.api_url(&format!("projects/{project_id}/merge_requests"))
        .to_string()
}

fn merge_request_url_for_project(base: &GitLabBaseUrl, project_id: &str, iid: u64) -> String {
    base.api_url(&format!("projects/{project_id}/merge_requests/{iid}"))
        .to_string()
}

fn merge_url_for_project(base: &GitLabBaseUrl, project_id: &str, iid: u64) -> String {
    base.api_url(&format!("projects/{project_id}/merge_requests/{iid}/merge"))
        .to_string()
}

fn gitlab_merge_squash_value(method: MergeStrategy) -> Result<bool> {
    match method {
        MergeStrategy::Merge => Ok(false),
        MergeStrategy::Squash => Ok(true),
        MergeStrategy::Rebase => Err(GitLabError::UnsupportedMergeStrategy { strategy: method }),
    }
}

async fn resolve_project_api_id(
    ctx: &GitLabContext,
    base: &GitLabBaseUrl,
    client: &fabro_http::HttpClient,
    repo: &GitLabRepository,
) -> Result<String> {
    const PER_PAGE: &str = "100";
    let mut page = 1_u32;

    loop {
        let page_string = page.to_string();
        let resp = send(
            client
                .get(base.api_url("projects"))
                .header("PRIVATE-TOKEN", token(ctx))
                .query(&[
                    ("simple", "true"),
                    ("per_page", PER_PAGE),
                    ("page", page_string.as_str()),
                    ("search", repo.repo.as_str()),
                ]),
            "Failed to resolve GitLab project",
        )
        .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = response_text(resp).await;
            return Err(GitLabError::Api(anyhow::anyhow!(
                "Unexpected status {status} resolving GitLab project {}: {body}",
                repo.full_path
            )));
        }

        let projects = resp
            .json::<Vec<GitLabProject>>()
            .await
            .map_err(|err| GitLabError::Api(anyhow::Error::new(err)))?;
        if let Some(project_id) = projects
            .iter()
            .find(|project| project.path_with_namespace == repo.full_path)
            .map(|project| project.id.to_string())
        {
            return Ok(project_id);
        }
        if projects.is_empty() {
            return Err(GitLabError::Api(anyhow::anyhow!(
                "GitLab project {} was not found in project search results",
                repo.full_path
            )));
        }

        page += 1;
    }
}

async fn send_project_request(
    ctx: &GitLabContext,
    base: &GitLabBaseUrl,
    client: &fabro_http::HttpClient,
    repo: &GitLabRepository,
    operation: &'static str,
    request: impl Fn(&str) -> fabro_http::RequestBuilder,
) -> Result<fabro_http::Response> {
    let resp = send(request(&repo.encoded_project_id), operation).await?;
    if resp.status() != fabro_http::StatusCode::NOT_FOUND {
        return Ok(resp);
    }

    let project_id = resolve_project_api_id(ctx, base, client, repo).await?;
    send(request(&project_id), operation).await
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
    let resp = send_project_request(
        ctx,
        &base,
        &client,
        repo,
        "Failed to create GitLab merge request",
        |project_id| {
            client
                .post(merge_requests_url_for_project(&base, project_id))
                .header("PRIVATE-TOKEN", token(ctx))
                .json(&body)
        },
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
    let resp = send_project_request(
        ctx,
        &base,
        &client,
        repo,
        "Failed to fetch GitLab merge request",
        |project_id| {
            client
                .get(merge_request_url_for_project(&base, project_id, iid))
                .header("PRIVATE-TOKEN", token(ctx))
        },
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
    let squash = gitlab_merge_squash_value(method)?;
    let resp = send_project_request(
        ctx,
        &base,
        &client,
        repo,
        "Failed to merge GitLab merge request",
        |project_id| {
            client
                .put(merge_url_for_project(&base, project_id, iid))
                .header("PRIVATE-TOKEN", token(ctx))
                .json(&serde_json::json!({ "squash": squash }))
        },
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
    let resp = send_project_request(
        ctx,
        &base,
        &client,
        repo,
        "Failed to close GitLab merge request",
        |project_id| {
            client
                .put(merge_request_url_for_project(&base, project_id, iid))
                .header("PRIVATE-TOKEN", token(ctx))
                .json(&CloseMergeRequestRequest::CLOSE)
        },
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
    let squash = gitlab_merge_squash_value(method)?;
    let resp = send_project_request(
        ctx,
        &base,
        &client,
        repo,
        "Failed to enable GitLab auto-merge",
        |project_id| {
            client
                .put(merge_url_for_project(&base, project_id, iid))
                .header("PRIVATE-TOKEN", token(ctx))
                .json(&serde_json::json!({
                    "auto_merge": true,
                    "merge_when_pipeline_succeeds": true,
                    "squash": squash,
                }))
        },
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
        additions: None,
        deletions: None,
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
    use fabro_types::settings::run::MergeStrategy;
    use httpmock::Method::{GET, POST};
    use httpmock::MockServer;

    use super::{
        CreateMergeRequestRequest, create_merge_request, merge_merge_request, parse_changes_count,
        to_pull_request_details,
    };
    use crate::repository::{GitLabBaseUrl, GitLabRepository, parse_origin};
    use crate::{GitLabContext, GitLabCredentials};

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

    #[test]
    fn maps_merge_request_without_line_stats() {
        let base = GitLabBaseUrl::parse("https://gitlab.example").unwrap();
        let repo = parse_origin(&base, "https://gitlab.example/platform/tools/fabro.git").unwrap();
        let mr = serde_json::from_value(merge_request_response(7)).unwrap();

        let details = to_pull_request_details(&repo, mr);

        assert_eq!(details.changed_files, 1);
        assert_eq!(details.additions, None);
        assert_eq!(details.deletions, None);
    }

    fn merge_request_response(iid: u64) -> serde_json::Value {
        serde_json::json!({
            "iid": iid,
            "id": iid * 100,
            "title": "Ship it",
            "description": "Body",
            "state": "opened",
            "web_url": format!("https://gitlab.example/platform/tools/fabro/-/merge_requests/{iid}"),
            "source_branch": "fabro/run-1",
            "target_branch": "main",
            "draft": true,
            "merge_status": "unchecked",
            "detailed_merge_status": "unchecked",
            "changes_count": "1",
            "author": {
                "id": 1,
                "username": "root",
                "name": "Administrator",
                "avatar_url": null,
                "web_url": "https://gitlab.example/root"
            },
            "created_at": "2026-06-17T12:00:00.000Z",
            "updated_at": "2026-06-17T12:00:00.000Z",
            "merged_at": null,
            "closed_at": null
        })
    }

    fn test_gitlab_context(server: &MockServer) -> (GitLabContext, GitLabRepository) {
        let base = GitLabBaseUrl::parse(&server.base_url()).unwrap();
        let repo = parse_origin(
            &base,
            &format!("{}/platform/tools/fabro.git", server.base_url()),
        )
        .unwrap();
        let ctx = GitLabContext {
            credentials: GitLabCredentials::Token("glpat-test".to_string()),
            base_url:    base.url.clone(),
            http_client: Some(fabro_http::test_http_client().unwrap()),
        };
        (ctx, repo)
    }

    #[tokio::test]
    async fn create_merge_request_posts_to_encoded_project_path_first() {
        let server = MockServer::start_async().await;
        let create = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v4/projects/platform%2Ftools%2Ffabro/merge_requests")
                .header("PRIVATE-TOKEN", "glpat-test")
                .json_body(serde_json::json!({
                    "source_branch": "fabro/run-1",
                    "target_branch": "main",
                    "title": "Ship it",
                    "description": "Body",
                    "draft": true,
                    "remove_source_branch": false
                }));
            then.status(201).json_body(merge_request_response(7));
        });
        let (ctx, repo) = test_gitlab_context(&server);

        let mr = create_merge_request(&ctx, &repo, "main", "fabro/run-1", "Ship it", "Body", true)
            .await
            .unwrap();

        create.assert();
        assert_eq!(mr.iid, 7);
    }

    #[tokio::test]
    async fn create_merge_request_falls_back_to_numeric_project_id_after_encoded_path_404() {
        let server = MockServer::start_async().await;
        let primary = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v4/projects/platform%2Ftools%2Ffabro/merge_requests")
                .header("PRIVATE-TOKEN", "glpat-test");
            then.status(404)
                .json_body(serde_json::json!({ "error": "404 Not Found" }));
        });
        let lookup = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v4/projects")
                .query_param("simple", "true")
                .query_param("per_page", "100")
                .query_param("page", "1")
                .query_param("search", "fabro")
                .header("PRIVATE-TOKEN", "glpat-test");
            then.status(200).json_body(serde_json::json!([{
                "id": 42,
                "path_with_namespace": "platform/tools/fabro"
            }]));
        });
        let create = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v4/projects/42/merge_requests")
                .header("PRIVATE-TOKEN", "glpat-test")
                .json_body(serde_json::json!({
                    "source_branch": "fabro/run-1",
                    "target_branch": "main",
                    "title": "Ship it",
                    "description": "Body",
                    "draft": true,
                    "remove_source_branch": false
                }));
            then.status(201).json_body(merge_request_response(7));
        });
        let (ctx, repo) = test_gitlab_context(&server);

        let mr = create_merge_request(&ctx, &repo, "main", "fabro/run-1", "Ship it", "Body", true)
            .await
            .unwrap();

        primary.assert();
        lookup.assert();
        create.assert();
        assert_eq!(mr.iid, 7);
    }

    #[tokio::test]
    async fn create_merge_request_project_id_fallback_searches_later_pages() {
        let server = MockServer::start_async().await;
        let primary = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v4/projects/platform%2Ftools%2Ffabro/merge_requests")
                .header("PRIVATE-TOKEN", "glpat-test");
            then.status(404)
                .json_body(serde_json::json!({ "error": "404 Not Found" }));
        });
        let first_page = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v4/projects")
                .query_param("simple", "true")
                .query_param("per_page", "100")
                .query_param("page", "1")
                .query_param("search", "fabro")
                .header("PRIVATE-TOKEN", "glpat-test");
            then.status(200).json_body(serde_json::json!([{
                "id": 11,
                "path_with_namespace": "acme/tools/fabro-helper"
            }]));
        });
        let second_page = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v4/projects")
                .query_param("simple", "true")
                .query_param("per_page", "100")
                .query_param("page", "2")
                .query_param("search", "fabro")
                .header("PRIVATE-TOKEN", "glpat-test");
            then.status(200).json_body(serde_json::json!([{
                "id": 42,
                "path_with_namespace": "platform/tools/fabro"
            }]));
        });
        let create = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v4/projects/42/merge_requests")
                .header("PRIVATE-TOKEN", "glpat-test");
            then.status(201).json_body(merge_request_response(7));
        });
        let (ctx, repo) = test_gitlab_context(&server);

        let mr = create_merge_request(&ctx, &repo, "main", "fabro/run-1", "Ship it", "Body", true)
            .await
            .unwrap();

        primary.assert();
        first_page.assert();
        second_page.assert();
        create.assert();
        assert_eq!(mr.iid, 7);
    }

    #[tokio::test]
    async fn create_merge_request_project_id_fallback_exhausts_project_search_pages() {
        let server = MockServer::start_async().await;
        let primary = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v4/projects/platform%2Ftools%2Ffabro/merge_requests")
                .header("PRIVATE-TOKEN", "glpat-test");
            then.status(404)
                .json_body(serde_json::json!({ "error": "404 Not Found" }));
        });
        let first_page = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v4/projects")
                .query_param("simple", "true")
                .query_param("per_page", "100")
                .query_param("page", "1")
                .query_param("search", "fabro")
                .header("PRIVATE-TOKEN", "glpat-test");
            then.status(200).json_body(serde_json::json!([{
                "id": 11,
                "path_with_namespace": "acme/tools/fabro-helper"
            }]));
        });
        let second_page = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v4/projects")
                .query_param("simple", "true")
                .query_param("per_page", "100")
                .query_param("page", "2")
                .query_param("search", "fabro")
                .header("PRIVATE-TOKEN", "glpat-test");
            then.status(200).json_body(serde_json::json!([]));
        });
        let (ctx, repo) = test_gitlab_context(&server);

        let err = create_merge_request(&ctx, &repo, "main", "fabro/run-1", "Ship it", "Body", true)
            .await
            .unwrap_err();

        primary.assert();
        first_page.assert();
        second_page.assert();
        let err = format!("{err:?}");
        assert!(
            err.contains(
                "GitLab project platform/tools/fabro was not found in project search results"
            ),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn merge_merge_request_rejects_rebase_before_calling_gitlab() {
        let server = MockServer::start_async().await;
        let (ctx, repo) = test_gitlab_context(&server);

        let err = merge_merge_request(&ctx, &repo, 7, MergeStrategy::Rebase)
            .await
            .unwrap_err();

        let err = err.to_string();
        assert!(
            err.contains("does not support rebase merge strategy"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn create_merge_request_falls_back_to_numeric_project_id_against_twin_gitlab() {
        let server = twin_gitlab::TestGitLabServer::start().await;
        server.add_user("alice", "Alice Example", "alice@example.test");
        server.add_project_with_id(10, "acme/tools/fabro-helper", "main", &[]);
        server.add_project_with_id(42, "platform/tools/fabro", "main", &["fabro/run-1"]);
        server.fail_project_path_refs_for("platform/tools/fabro");

        let base = GitLabBaseUrl::parse(server.base_url()).unwrap();
        let repo = parse_origin(
            &base,
            &format!("{}/platform/tools/fabro.git", server.base_url()),
        )
        .unwrap();
        let ctx = GitLabContext {
            credentials: GitLabCredentials::Token(server.automation_token().to_string()),
            base_url:    base.url.clone(),
            http_client: Some(fabro_http::test_http_client().unwrap()),
        };

        let mr = create_merge_request(&ctx, &repo, "main", "fabro/run-1", "Ship it", "Body", false)
            .await
            .unwrap();

        assert_eq!(mr.iid, 1);
        assert_eq!(
            mr.web_url,
            format!(
                "{}/platform/tools/fabro/-/merge_requests/1",
                server.base_url()
            )
        );
    }
}
