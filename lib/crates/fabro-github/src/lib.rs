use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::OnceCell;

pub use fabro_tracker::{BlockerRef, Issue, Tracker};

pub const GITHUB_API_BASE_URL: &str = "https://api.github.com";

/// Detailed information about a pull request from the GitHub API.
#[derive(Debug, Clone, Deserialize)]
pub struct PullRequestDetail {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub draft: bool,
    pub mergeable: Option<bool>,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub html_url: String,
    pub user: PullRequestUser,
    pub head: PullRequestRef,
    pub base: PullRequestRef,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullRequestUser {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullRequestRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
}

/// Credentials for authenticating as a GitHub App.
#[derive(Clone, Debug)]
pub struct GitHubAppCredentials {
    pub app_id: String,
    pub private_key_pem: String,
}

/// Parse `owner` and `repo` from a GitHub HTTPS URL.
///
/// Accepts URLs like:
/// - `https://github.com/owner/repo.git`
/// - `https://github.com/owner/repo`
/// - `https://github.com/owner/repo/`
/// - `https://x-access-token:TOKEN@github.com/owner/repo.git`
pub fn parse_github_owner_repo(url: &str) -> Result<(String, String), String> {
    // Strip credentials from URLs like https://x-access-token:TOKEN@github.com/...
    let stripped = url.strip_prefix("https://").and_then(|rest| {
        rest.split_once('@')
            .map(|(_, after)| format!("https://{after}"))
    });
    let url = stripped.as_deref().unwrap_or(url);
    let path = url
        .strip_prefix("https://github.com/")
        .ok_or_else(|| format!("Not a GitHub HTTPS URL: {url}"))?;

    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);

    let mut parts = path.splitn(3, '/');
    let owner = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("Missing owner in GitHub URL: {url}"))?;
    let repo = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("Missing repo in GitHub URL: {url}"))?;

    Ok((owner.to_string(), repo.to_string()))
}

/// Create a signed JWT for GitHub App authentication (RS256).
///
/// The JWT is valid for 10 minutes with a 60-second clock skew allowance.
pub fn sign_app_jwt(app_id: &str, private_key_pem: &str) -> Result<String, String> {
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    use serde::Serialize;

    #[derive(Serialize)]
    struct Claims {
        iss: String,
        iat: i64,
        exp: i64,
    }

    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        iss: app_id.to_string(),
        iat: now - 60,
        exp: now + 600,
    };

    let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .map_err(|e| format!("Invalid RSA private key: {e}"))?;

    let jwt = encode(&Header::new(Algorithm::RS256), &claims, &key)
        .map_err(|e| format!("Failed to sign JWT: {e}"))?;
    Ok(jwt)
}

/// Request a scoped Installation Access Token for a specific repository.
///
/// Uses the App JWT to find the installation for `owner/repo`, then requests
/// a token scoped to the given `permissions` on that single repository.
pub async fn create_installation_access_token_with_permissions(
    client: &reqwest::Client,
    jwt: &str,
    owner: &str,
    repo: &str,
    base_url: &str,
    permissions: serde_json::Value,
) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Installation {
        id: u64,
    }

    #[derive(Deserialize)]
    struct AccessToken {
        token: String,
    }

    // Step 1: Find the installation for this repo
    let install_url = format!("{base_url}/repos/{owner}/{repo}/installation");
    let install_resp = client
        .get(&install_url)
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro")
        .send()
        .await
        .map_err(|e| format!("Failed to look up GitHub App installation: {e}"))?;

    let status = install_resp.status();
    match status.as_u16() {
        200 => {}
        404 => {
            return Err(format!(
                "GitHub App is not installed for {owner}. \
                 Install it at https://github.com/organizations/{owner}/settings/installations"
            ));
        }
        403 => {
            return Err("GitHub App installation is suspended. \
                 Re-enable it in your organization's GitHub App settings."
                .to_string());
        }
        401 => {
            return Err("GitHub App authentication failed. \
                 Check that app_id and GITHUB_APP_PRIVATE_KEY are correct."
                .to_string());
        }
        _ => {
            return Err(format!(
                "Unexpected status {status} looking up GitHub App installation"
            ));
        }
    }

    let installation: Installation = install_resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse installation response: {e}"))?;

    // Step 2: Create a scoped access token
    let token_url = format!(
        "{base_url}/app/installations/{}/access_tokens",
        installation.id
    );
    let body = serde_json::json!({
        "repositories": [repo],
        "permissions": permissions,
    });

    let token_resp = client
        .post(&token_url)
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Failed to create installation access token: {e}"))?;

    let token_status = token_resp.status();
    match token_status.as_u16() {
        201 => {}
        422 => {
            return Err(format!(
                "GitHub App does not have access to repository {repo}. \
                 Update the installation's repository permissions to include it."
            ));
        }
        401 => {
            return Err("GitHub App authentication failed. \
                 Check that app_id and GITHUB_APP_PRIVATE_KEY are correct."
                .to_string());
        }
        _ => {
            return Err(format!(
                "Unexpected status {token_status} creating installation access token"
            ));
        }
    }

    let access_token: AccessToken = token_resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse access token response: {e}"))?;

    Ok(access_token.token)
}

/// Request a scoped Installation Access Token with `contents: write`.
pub async fn create_installation_access_token(
    client: &reqwest::Client,
    jwt: &str,
    owner: &str,
    repo: &str,
    base_url: &str,
) -> Result<String, String> {
    create_installation_access_token_with_permissions(
        client,
        jwt,
        owner,
        repo,
        base_url,
        serde_json::json!({ "contents": "write" }),
    )
    .await
}

/// Request a scoped Installation Access Token with `contents: write`
/// and `pull_requests: write`. Used for creating pull requests.
pub async fn create_installation_access_token_for_pr(
    client: &reqwest::Client,
    jwt: &str,
    owner: &str,
    repo: &str,
    base_url: &str,
) -> Result<String, String> {
    create_installation_access_token_with_permissions(
        client,
        jwt,
        owner,
        repo,
        base_url,
        serde_json::json!({ "contents": "write", "pull_requests": "write" }),
    )
    .await
}

/// Result of a successful pull request creation.
pub struct CreatedPullRequest {
    pub html_url: String,
    pub number: u64,
    pub node_id: String,
}

/// Create a pull request on GitHub.
///
/// Signs a JWT, obtains a PR-scoped installation token, and POSTs to the
/// GitHub pulls API.
#[allow(clippy::too_many_arguments)]
pub async fn create_pull_request(
    creds: &GitHubAppCredentials,
    owner: &str,
    repo: &str,
    base: &str,
    head: &str,
    title: &str,
    body: &str,
    draft: bool,
) -> Result<CreatedPullRequest, String> {
    let jwt = sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
    let client = reqwest::Client::new();

    let token =
        create_installation_access_token_for_pr(&client, &jwt, owner, repo, GITHUB_API_BASE_URL)
            .await?;

    tracing::debug!(title = %title, head = %head, base = %base, draft, "Creating pull request");

    let pr_body = serde_json::json!({
        "title": title,
        "head": head,
        "base": base,
        "body": body,
        "draft": draft,
    });

    let url = format!("{GITHUB_API_BASE_URL}/repos/{owner}/{repo}/pulls");
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro")
        .json(&pr_body)
        .send()
        .await
        .map_err(|e| format!("Failed to create pull request: {e}"))?;

    let status = resp.status();
    match status.as_u16() {
        201 => {}
        422 => {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(format!(
                "Pull request could not be created (422): {body_text}"
            ));
        }
        401 | 403 => {
            return Err(format!(
                "Authentication failed creating pull request ({})",
                status
            ));
        }
        _ => {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(format!(
                "Unexpected status {status} creating pull request: {body_text}"
            ));
        }
    }

    #[derive(Deserialize)]
    struct PullRequestResponse {
        html_url: String,
        number: u64,
        node_id: String,
    }

    let pr: PullRequestResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse pull request response: {e}"))?;

    Ok(CreatedPullRequest {
        html_url: pr.html_url,
        number: pr.number,
        node_id: pr.node_id,
    })
}

/// GitHub GraphQL merge method for auto-merge.
#[derive(Clone, Copy, Debug)]
pub enum AutoMergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl AutoMergeMethod {
    fn as_graphql_value(self) -> &'static str {
        match self {
            Self::Merge => "MERGE",
            Self::Squash => "SQUASH",
            Self::Rebase => "REBASE",
        }
    }
}

/// Enable auto-merge on a pull request via GitHub's GraphQL API.
///
/// Requires the PR's `node_id` (from the REST API response) and a merge method.
/// The repository must have auto-merge enabled in its settings.
pub async fn enable_auto_merge(
    creds: &GitHubAppCredentials,
    owner: &str,
    repo: &str,
    pr_node_id: &str,
    merge_method: AutoMergeMethod,
) -> Result<(), String> {
    let jwt = sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
    let client = reqwest::Client::new();

    let token =
        create_installation_access_token_for_pr(&client, &jwt, owner, repo, GITHUB_API_BASE_URL)
            .await?;

    let query = format!(
        r#"mutation {{
  enablePullRequestAutoMerge(input: {{pullRequestId: "{pr_node_id}", mergeMethod: {merge_method}}}) {{
    pullRequest {{
      autoMergeRequest {{
        enabledAt
        mergeMethod
      }}
    }}
  }}
}}"#,
        merge_method = merge_method.as_graphql_value(),
    );

    tracing::debug!(
        pr_node_id,
        merge_method = merge_method.as_graphql_value(),
        "Enabling auto-merge"
    );

    let resp = client
        .post(format!("{GITHUB_API_BASE_URL}/graphql"))
        .header("Authorization", format!("Bearer {token}"))
        .header("User-Agent", "fabro")
        .json(&serde_json::json!({ "query": query }))
        .send()
        .await
        .map_err(|e| format!("Failed to enable auto-merge: {e}"))?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse auto-merge response: {e}"))?;

    if !status.is_success() {
        return Err(format!("Auto-merge request failed ({status}): {body}"));
    }

    if let Some(errors) = body.get("errors") {
        return Err(format!("Auto-merge GraphQL error: {errors}"));
    }

    tracing::info!(pr_node_id, "Auto-merge enabled");
    Ok(())
}

/// Convert a Git SSH URL to HTTPS format for token-based authentication.
///
/// SSH URLs like `git@github.com:owner/repo.git` become
/// `https://github.com/owner/repo.git`. URLs that are already HTTPS
/// (or any other non-SSH format) are returned unchanged.
pub fn ssh_url_to_https(url: &str) -> String {
    // Match `git@<host>:<path>` (standard SSH URL format)
    if let Some(rest) = url.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            return format!("https://{host}/{path}");
        }
    }
    // Match `ssh://git@<host>/<path>`
    if let Some(rest) = url.strip_prefix("ssh://git@") {
        return format!("https://{rest}");
    }
    url.to_string()
}

/// Check whether a branch exists in a GitHub repository.
///
/// Uses a GitHub App installation token to query the branches API.
/// Returns `true` if the branch exists, `false` if it doesn't (404).
pub async fn branch_exists(
    creds: &GitHubAppCredentials,
    owner: &str,
    repo: &str,
    branch: &str,
    base_url: &str,
) -> Result<bool, String> {
    let jwt = sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
    let client = reqwest::Client::new();

    let token = create_installation_access_token(&client, &jwt, owner, repo, base_url).await?;

    let url = format!("{base_url}/repos/{owner}/{repo}/branches/{branch}");
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro")
        .send()
        .await
        .map_err(|e| format!("Failed to check branch existence: {e}"))?;

    match resp.status().as_u16() {
        200 => Ok(true),
        404 => Ok(false),
        status => Err(format!(
            "Unexpected status {status} checking branch '{branch}'"
        )),
    }
}

/// Check whether a GitHub App is installed for a specific repository.
///
/// Uses the App JWT to query `GET /repos/{owner}/{repo}/installation`.
/// Returns `Ok(true)` on 200, `Ok(false)` on 404.
pub async fn check_app_installed(
    client: &reqwest::Client,
    jwt: &str,
    owner: &str,
    repo: &str,
    base_url: &str,
) -> Result<bool, String> {
    let url = format!("{base_url}/repos/{owner}/{repo}/installation");
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro")
        .send()
        .await
        .map_err(|e| format!("Failed to check GitHub App installation: {e}"))?;

    match resp.status().as_u16() {
        200 => Ok(true),
        404 => Ok(false),
        401 => Err("GitHub App authentication failed. \
             Check that app_id and GITHUB_APP_PRIVATE_KEY are correct."
            .to_string()),
        403 => Err("GitHub App installation is suspended. \
             Re-enable it in your organization's GitHub App settings."
            .to_string()),
        status => Err(format!(
            "Unexpected status {status} checking GitHub App installation"
        )),
    }
}

/// Resolve git clone credentials for a GitHub repository.
///
/// Returns `(username, password)` for authenticated cloning.
/// Always generates a token regardless of repo visibility, since the token
/// is needed for pushing from the sandbox.
pub async fn resolve_clone_credentials(
    creds: &GitHubAppCredentials,
    owner: &str,
    repo: &str,
) -> Result<(Option<String>, Option<String>), String> {
    let jwt = sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
    let client = reqwest::Client::new();

    let token =
        create_installation_access_token(&client, &jwt, owner, repo, GITHUB_API_BASE_URL).await?;
    Ok((Some("x-access-token".to_string()), Some(token)))
}

/// Embed a token into an HTTPS URL for authenticated git operations.
///
/// Converts `https://github.com/owner/repo` to
/// `https://x-access-token:<token>@github.com/owner/repo`.
pub fn embed_token_in_url(url: &str, token: &str) -> String {
    url.replacen("https://", &format!("https://x-access-token:{token}@"), 1)
}

/// Resolve an authenticated HTTPS URL for a GitHub repository.
///
/// Parses owner/repo from the URL, obtains a fresh installation access token,
/// and returns the URL with embedded credentials. Returns the original URL
/// unchanged if it's not a GitHub URL.
pub async fn resolve_authenticated_url(
    creds: &GitHubAppCredentials,
    url: &str,
) -> Result<String, String> {
    let (owner, repo) = parse_github_owner_repo(url)?;
    let (_username, password) = resolve_clone_credentials(creds, &owner, &repo).await?;
    match password {
        Some(token) => Ok(embed_token_in_url(url, &token)),
        None => Ok(url.to_string()),
    }
}

/// Fetch detailed information about a pull request.
pub async fn get_pull_request(
    creds: &GitHubAppCredentials,
    owner: &str,
    repo: &str,
    number: u64,
    base_url: &str,
) -> Result<PullRequestDetail, String> {
    tracing::debug!(owner, repo, number, "Fetching pull request");

    let jwt = sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
    let client = reqwest::Client::new();
    let token =
        create_installation_access_token_for_pr(&client, &jwt, owner, repo, base_url).await?;

    let url = format!("{base_url}/repos/{owner}/{repo}/pulls/{number}");
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch pull request: {e}"))?;

    match resp.status().as_u16() {
        200 => {}
        404 => {
            return Err(format!(
                "Pull request #{number} not found in {owner}/{repo}"
            ))
        }
        401 | 403 => {
            return Err(format!(
                "Authentication failed fetching pull request ({})",
                resp.status()
            ))
        }
        status => {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "Unexpected status {status} fetching pull request: {body}"
            ));
        }
    }

    resp.json::<PullRequestDetail>()
        .await
        .map_err(|e| format!("Failed to parse pull request response: {e}"))
}

/// Merge a pull request.
pub async fn merge_pull_request(
    creds: &GitHubAppCredentials,
    owner: &str,
    repo: &str,
    number: u64,
    method: &str,
    base_url: &str,
) -> Result<(), String> {
    tracing::debug!(owner, repo, number, method, "Merging pull request");

    let jwt = sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
    let client = reqwest::Client::new();
    let token =
        create_installation_access_token_for_pr(&client, &jwt, owner, repo, base_url).await?;

    let url = format!("{base_url}/repos/{owner}/{repo}/pulls/{number}/merge");
    let body = serde_json::json!({ "merge_method": method });

    let resp = client
        .put(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Failed to merge pull request: {e}"))?;

    match resp.status().as_u16() {
        200 => Ok(()),
        405 => Err(format!(
            "Pull request #{number} is not mergeable (method may not be allowed)"
        )),
        409 => Err(format!("Pull request #{number} has a merge conflict")),
        404 => Err(format!(
            "Pull request #{number} not found in {owner}/{repo}"
        )),
        401 | 403 => Err(format!(
            "Authentication failed merging pull request ({})",
            resp.status()
        )),
        status => {
            let body_text = resp.text().await.unwrap_or_default();
            Err(format!(
                "Unexpected status {status} merging pull request: {body_text}"
            ))
        }
    }
}

/// Close a pull request.
pub async fn close_pull_request(
    creds: &GitHubAppCredentials,
    owner: &str,
    repo: &str,
    number: u64,
    base_url: &str,
) -> Result<(), String> {
    tracing::debug!(owner, repo, number, "Closing pull request");

    let jwt = sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
    let client = reqwest::Client::new();
    let token =
        create_installation_access_token_for_pr(&client, &jwt, owner, repo, base_url).await?;

    let url = format!("{base_url}/repos/{owner}/{repo}/pulls/{number}");
    let body = serde_json::json!({ "state": "closed" });

    let resp = client
        .patch(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Failed to close pull request: {e}"))?;

    match resp.status().as_u16() {
        200 => Ok(()),
        404 => Err(format!(
            "Pull request #{number} not found in {owner}/{repo}"
        )),
        401 | 403 => Err(format!(
            "Authentication failed closing pull request ({})",
            resp.status()
        )),
        status => {
            let body_text = resp.text().await.unwrap_or_default();
            Err(format!(
                "Unexpected status {status} closing pull request: {body_text}"
            ))
        }
    }
}

/// Execute a GitHub GraphQL request and return the response JSON.
async fn execute_github_graphql(
    client: &reqwest::Client,
    token: &str,
    endpoint: &str,
    query: &str,
    variables: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({
        "query": query,
        "variables": variables,
    });

    let resp = client
        .post(endpoint)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", "fabro")
        .timeout(std::time::Duration::from_secs(30))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("GitHub GraphQL request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        tracing::warn!(status = %status, "GitHub GraphQL API error");
        return Err(format!(
            "GitHub GraphQL API returned HTTP {status}: {body_text}"
        ));
    }

    let response: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse GitHub GraphQL response: {e}"))?;

    if let Some(errors) = response["errors"].as_array() {
        if !errors.is_empty() {
            let messages: Vec<&str> = errors
                .iter()
                .filter_map(|e| e["message"].as_str())
                .collect();
            return Err(format!("GitHub GraphQL errors: {}", messages.join("; ")));
        }
    }

    Ok(response)
}

/// Request a scoped Installation Access Token with `issues: write`
/// and `organization_projects: write`. Used for GitHub Projects V2.
pub async fn create_installation_access_token_for_projects(
    client: &reqwest::Client,
    jwt: &str,
    owner: &str,
    repo: &str,
    base_url: &str,
) -> Result<String, String> {
    create_installation_access_token_with_permissions(
        client,
        jwt,
        owner,
        repo,
        base_url,
        serde_json::json!({ "issues": "write", "organization_projects": "write" }),
    )
    .await
}

/// A `Tracker` implementation backed by GitHub Projects V2.
///
/// Scoped to a single project board identified by `project_number`.
pub struct GitHubTracker {
    creds: GitHubAppCredentials,
    client: reqwest::Client,
    owner: String,
    repo: String,
    project_number: u64,
    base_url: String,
    project_node_id: OnceCell<String>,
}

impl GitHubTracker {
    pub fn new(
        creds: GitHubAppCredentials,
        client: reqwest::Client,
        owner: String,
        repo: String,
        project_number: u64,
        base_url: String,
    ) -> Self {
        Self {
            creds,
            client,
            owner,
            repo,
            project_number,
            base_url,
            project_node_id: OnceCell::new(),
        }
    }

    fn graphql_url(&self) -> String {
        format!("{}/graphql", self.base_url)
    }

    async fn fresh_token(&self) -> Result<String, String> {
        let jwt = sign_app_jwt(&self.creds.app_id, &self.creds.private_key_pem)?;
        create_installation_access_token_for_projects(
            &self.client,
            &jwt,
            &self.owner,
            &self.repo,
            &self.base_url,
        )
        .await
    }

    async fn resolve_project_node_id(&self, token: &str) -> Result<&str, String> {
        self.project_node_id
            .get_or_try_init(|| async {
                tracing::debug!(
                    owner = %self.owner,
                    project_number = self.project_number,
                    "Resolving GitHub project node ID"
                );
                let graphql_url = self.graphql_url();
                let query = r#"
                    query($owner: String!, $number: Int!) {
                        organization(login: $owner) {
                            projectV2(number: $number) { id }
                        }
                    }
                "#;
                let variables = serde_json::json!({
                    "owner": self.owner,
                    "number": self.project_number,
                });

                let resp = execute_github_graphql(
                    &self.client,
                    token,
                    &graphql_url,
                    query,
                    variables.clone(),
                )
                .await?;

                // Try org path first, fall back to user path
                if let Some(id) = resp["data"]["organization"]["projectV2"]["id"].as_str() {
                    return Ok(id.to_string());
                }

                let user_query = r#"
                    query($owner: String!, $number: Int!) {
                        user(login: $owner) {
                            projectV2(number: $number) { id }
                        }
                    }
                "#;
                let user_resp = execute_github_graphql(
                    &self.client,
                    token,
                    &graphql_url,
                    user_query,
                    variables,
                )
                .await?;

                user_resp["data"]["user"]["projectV2"]["id"]
                    .as_str()
                    .map(|s| s.to_string())
                    .ok_or_else(|| {
                        format!(
                            "Project #{} not found for owner '{}'",
                            self.project_number, self.owner
                        )
                    })
            })
            .await
            .map(|s| s.as_str())
    }
}

fn normalize_github_item(item: &serde_json::Value) -> Option<Issue> {
    let project_item_id = item["id"].as_str()?.to_string();
    let content = &item["content"];

    let id = content["id"].as_str()?.to_string();
    let number = content["number"].as_u64()?;
    let identifier = format!("#{number}");
    let title = content["title"].as_str()?.to_string();
    let url = content["url"].as_str()?.to_string();
    let description = content["body"].as_str().map(|s| s.to_string());

    let state = item["fieldValueByName"]["name"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let assignee_id = content["assignees"]["nodes"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|a| a["id"].as_str())
        .map(|s| s.to_string());

    let labels = content["labels"]["nodes"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l["name"].as_str())
                .map(|s| s.to_lowercase())
                .collect()
        })
        .unwrap_or_default();

    let created_at = content["createdAt"].as_str().map(|s| s.to_string());
    let updated_at = content["updatedAt"].as_str().map(|s| s.to_string());

    Some(Issue {
        id,
        project_item_id: Some(project_item_id),
        identifier,
        title,
        description,
        priority: None,
        state,
        branch_name: None,
        url,
        assignee_id,
        labels,
        blocked_by: vec![],
        created_at,
        updated_at,
    })
}

/// Fetch one page of project items. Returns (items, has_next_page, end_cursor).
async fn fetch_project_items_page(
    client: &reqwest::Client,
    token: &str,
    graphql_url: &str,
    project_node_id: &str,
    cursor: Option<&str>,
) -> Result<(Vec<serde_json::Value>, bool, Option<String>), String> {
    let query = r#"
        query($projectId: ID!, $cursor: String) {
            node(id: $projectId) {
                ... on ProjectV2 {
                    items(first: 100, after: $cursor) {
                        nodes {
                            id
                            fieldValueByName(name: "Status") {
                                ... on ProjectV2ItemFieldSingleSelectValue {
                                    name
                                }
                            }
                            content {
                                ... on Issue {
                                    id
                                    number
                                    title
                                    body
                                    url
                                    createdAt
                                    updatedAt
                                    assignees(first: 1) { nodes { id } }
                                    labels(first: 20) { nodes { name } }
                                }
                            }
                        }
                        pageInfo { hasNextPage endCursor }
                    }
                }
            }
        }
    "#;

    let variables = serde_json::json!({
        "projectId": project_node_id,
        "cursor": cursor,
    });

    let resp = execute_github_graphql(client, token, graphql_url, query, variables).await?;

    let items_node = &resp["data"]["node"]["items"];
    let nodes = items_node["nodes"].as_array().cloned().unwrap_or_default();
    let has_next = items_node["pageInfo"]["hasNextPage"]
        .as_bool()
        .unwrap_or(false);
    let end_cursor = items_node["pageInfo"]["endCursor"]
        .as_str()
        .map(|s| s.to_string());

    Ok((nodes, has_next, end_cursor))
}

#[async_trait]
impl Tracker for GitHubTracker {
    async fn fetch_viewer_id(&self) -> Result<String, String> {
        tracing::debug!("Fetching viewer ID from GitHub");
        let token = self.fresh_token().await?;
        let query = "query { viewer { id } }";
        let resp = execute_github_graphql(
            &self.client,
            &token,
            &self.graphql_url(),
            query,
            serde_json::json!({}),
        )
        .await?;

        resp["data"]["viewer"]["id"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| "Missing viewer id in GitHub response".to_string())
    }

    async fn create_comment(&self, issue: &Issue, body: &str) -> Result<(), String> {
        tracing::debug!(issue_id = %issue.id, "Creating comment on GitHub issue");
        let token = self.fresh_token().await?;
        let query = r#"
            mutation($subjectId: ID!, $body: String!) {
                addComment(input: { subjectId: $subjectId, body: $body }) {
                    clientMutationId
                }
            }
        "#;
        let variables = serde_json::json!({
            "subjectId": issue.id,
            "body": body,
        });
        execute_github_graphql(&self.client, &token, &self.graphql_url(), query, variables).await?;
        Ok(())
    }

    async fn update_issue_state(&self, issue: &Issue, state_name: &str) -> Result<(), String> {
        let project_item_id = issue
            .project_item_id
            .as_deref()
            .ok_or("update_issue_state requires project_item_id")?;

        tracing::debug!(
            project_item_id,
            state_name,
            "Updating GitHub project item status"
        );

        let token = self.fresh_token().await?;
        let project_node_id = self.resolve_project_node_id(&token).await?;
        let graphql_url = self.graphql_url();

        // Step 1: Get the Status field ID and the target option ID
        let field_query = r#"
            query($projectId: ID!) {
                node(id: $projectId) {
                    ... on ProjectV2 {
                        field(name: "Status") {
                            ... on ProjectV2SingleSelectField {
                                id
                                options { id name }
                            }
                        }
                    }
                }
            }
        "#;
        let field_resp = execute_github_graphql(
            &self.client,
            &token,
            &graphql_url,
            field_query,
            serde_json::json!({ "projectId": project_node_id }),
        )
        .await?;

        let field = &field_resp["data"]["node"]["field"];
        let field_id = field["id"]
            .as_str()
            .ok_or("Missing Status field id")?
            .to_string();

        let option_id = field["options"]
            .as_array()
            .and_then(|opts| {
                opts.iter().find(|o| {
                    o["name"]
                        .as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(state_name))
                })
            })
            .and_then(|o| o["id"].as_str())
            .ok_or_else(|| format!("Status option '{state_name}' not found in project"))?
            .to_string();

        // Step 2: Update the field value
        let update_query = r#"
            mutation($projectId: ID!, $itemId: ID!, $fieldId: ID!, $optionId: String!) {
                updateProjectV2ItemFieldValue(input: {
                    projectId: $projectId
                    itemId: $itemId
                    fieldId: $fieldId
                    value: { singleSelectOptionId: $optionId }
                }) {
                    projectV2Item { id }
                }
            }
        "#;
        execute_github_graphql(
            &self.client,
            &token,
            &graphql_url,
            update_query,
            serde_json::json!({
                "projectId": project_node_id,
                "itemId": project_item_id,
                "fieldId": field_id,
                "optionId": option_id,
            }),
        )
        .await?;

        Ok(())
    }

    async fn fetch_candidate_issues(&self, state_names: &[&str]) -> Result<Vec<Issue>, String> {
        tracing::debug!(
            owner = %self.owner,
            project_number = self.project_number,
            ?state_names,
            "Fetching candidate issues from GitHub project"
        );

        let token = self.fresh_token().await?;
        let project_node_id = self.resolve_project_node_id(&token).await?;
        let graphql_url = self.graphql_url();

        let mut all_issues = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let (nodes, has_next, end_cursor) = fetch_project_items_page(
                &self.client,
                &token,
                &graphql_url,
                project_node_id,
                cursor.as_deref(),
            )
            .await?;

            for node in &nodes {
                if let Some(issue) = normalize_github_item(node) {
                    if state_names
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(&issue.state))
                    {
                        all_issues.push(issue);
                    }
                }
            }

            if has_next {
                cursor = end_cursor;
            } else {
                break;
            }
        }

        Ok(all_issues)
    }

    async fn fetch_issues_by_ids(&self, ids: &[&str]) -> Result<Vec<Issue>, String> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        tracing::debug!(
            count = ids.len(),
            "Fetching GitHub issues by ID from project"
        );

        let token = self.fresh_token().await?;
        let project_node_id = self.resolve_project_node_id(&token).await?;
        let graphql_url = self.graphql_url();

        let id_set: std::collections::HashSet<&str> = ids.iter().copied().collect();
        let mut issue_map: std::collections::HashMap<String, Issue> =
            std::collections::HashMap::new();
        let mut cursor: Option<String> = None;

        loop {
            let (nodes, has_next, end_cursor) = fetch_project_items_page(
                &self.client,
                &token,
                &graphql_url,
                project_node_id,
                cursor.as_deref(),
            )
            .await?;

            for node in &nodes {
                if let Some(issue) = normalize_github_item(node) {
                    if id_set.contains(issue.id.as_str()) {
                        issue_map.insert(issue.id.clone(), issue);
                    }
                }
            }

            if has_next {
                cursor = end_cursor;
            } else {
                break;
            }
        }

        // Return in the same order as the input IDs
        Ok(ids.iter().filter_map(|id| issue_map.remove(*id)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // parse_github_owner_repo
    // -----------------------------------------------------------------------

    #[test]
    fn parse_https_with_git_suffix() {
        let (owner, repo) = parse_github_owner_repo("https://github.com/owner/repo.git").unwrap();
        assert_eq!(owner, "owner");
        assert_eq!(repo, "repo");
    }

    #[test]
    fn parse_https_without_git_suffix() {
        let (owner, repo) = parse_github_owner_repo("https://github.com/owner/repo").unwrap();
        assert_eq!(owner, "owner");
        assert_eq!(repo, "repo");
    }

    #[test]
    fn parse_https_with_trailing_slash() {
        let (owner, repo) = parse_github_owner_repo("https://github.com/owner/repo/").unwrap();
        assert_eq!(owner, "owner");
        assert_eq!(repo, "repo");
    }

    // -----------------------------------------------------------------------
    // ssh_url_to_https
    // -----------------------------------------------------------------------

    #[test]
    fn ssh_url_to_https_converts_git_at_syntax() {
        assert_eq!(
            ssh_url_to_https("git@github.com:brynary/arc.git"),
            "https://github.com/brynary/arc.git"
        );
    }

    #[test]
    fn ssh_url_to_https_converts_ssh_protocol() {
        assert_eq!(
            ssh_url_to_https("ssh://git@github.com/brynary/arc.git"),
            "https://github.com/brynary/arc.git"
        );
    }

    #[test]
    fn ssh_url_to_https_passes_through_https() {
        assert_eq!(
            ssh_url_to_https("https://github.com/brynary/arc.git"),
            "https://github.com/brynary/arc.git"
        );
    }

    #[test]
    fn parse_github_url_with_credentials() {
        let (owner, repo) = parse_github_owner_repo(
            "https://x-access-token:ghs_abc123@github.com/acme/widgets.git",
        )
        .unwrap();
        assert_eq!(owner, "acme");
        assert_eq!(repo, "widgets");
    }

    #[test]
    fn parse_github_url_with_credentials_no_password() {
        let (owner, repo) =
            parse_github_owner_repo("https://token@github.com/acme/widgets.git").unwrap();
        assert_eq!(owner, "acme");
        assert_eq!(repo, "widgets");
    }

    #[test]
    fn parse_credentials_non_github_still_errors() {
        let result = parse_github_owner_repo("https://user:pass@gitlab.com/owner/repo");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Not a GitHub HTTPS URL"));
    }

    #[test]
    fn parse_non_github_url_errors() {
        let result = parse_github_owner_repo("https://gitlab.com/owner/repo");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Not a GitHub HTTPS URL"));
    }

    #[test]
    fn parse_missing_repo_errors() {
        let result = parse_github_owner_repo("https://github.com/owner");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Missing repo"));
    }

    #[test]
    fn parse_empty_string_errors() {
        let result = parse_github_owner_repo("");
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // sign_app_jwt
    // -----------------------------------------------------------------------

    fn test_rsa_key() -> String {
        use std::process::Command;
        let output = Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
            ])
            .output()
            .expect("openssl should be available");
        assert!(output.status.success(), "openssl keygen failed");
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn jwt_is_three_part_string() {
        let pem = test_rsa_key();
        let jwt = sign_app_jwt("12345", &pem).unwrap();
        assert_eq!(jwt.split('.').count(), 3);
    }

    #[test]
    fn jwt_has_rs256_header() {
        let pem = test_rsa_key();
        let jwt = sign_app_jwt("12345", &pem).unwrap();
        let header_b64 = jwt.split('.').next().unwrap();
        let header_json = base64::Engine::decode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            header_b64,
        )
        .unwrap();
        let header: serde_json::Value = serde_json::from_slice(&header_json).unwrap();
        assert_eq!(header["alg"], "RS256");
    }

    #[test]
    fn jwt_has_correct_claims() {
        let pem = test_rsa_key();
        let jwt = sign_app_jwt("99999", &pem).unwrap();
        let payload_b64 = jwt.split('.').nth(1).unwrap();
        let payload_json = base64::Engine::decode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            payload_b64,
        )
        .unwrap();
        let claims: serde_json::Value = serde_json::from_slice(&payload_json).unwrap();
        assert_eq!(claims["iss"], "99999");

        let now = chrono::Utc::now().timestamp();
        let iat = claims["iat"].as_i64().unwrap();
        let exp = claims["exp"].as_i64().unwrap();
        // iat should be ~60s before now
        assert!((now - 60 - iat).abs() < 5);
        // exp should be ~10min after now
        assert!((now + 600 - exp).abs() < 5);
    }

    #[test]
    fn jwt_invalid_pem_errors() {
        let result = sign_app_jwt("12345", "not-a-pem");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid RSA private key"));
    }

    // -----------------------------------------------------------------------
    // create_installation_access_token — success
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_iat_success() {
        let mut server = mockito::Server::new_async().await;

        let install_mock = server
            .mock("GET", "/repos/owner/repo/installation")
            .match_header("Authorization", "Bearer test-jwt")
            .with_status(200)
            .with_body(r#"{"id": 123}"#)
            .create_async()
            .await;

        let token_mock = server
            .mock("POST", "/app/installations/123/access_tokens")
            .match_header("Authorization", "Bearer test-jwt")
            .match_body(mockito::Matcher::JsonString(
                r#"{"repositories":["repo"],"permissions":{"contents":"write"}}"#.to_string(),
            ))
            .with_status(201)
            .with_body(r#"{"token": "ghs_xxx"}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let token =
            create_installation_access_token(&client, "test-jwt", "owner", "repo", &server.url())
                .await
                .unwrap();
        assert_eq!(token, "ghs_xxx");

        install_mock.assert_async().await;
        token_mock.assert_async().await;
    }

    // -----------------------------------------------------------------------
    // create_installation_access_token — failure modes
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_iat_not_installed() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(404)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let err = create_installation_access_token(&client, "jwt", "owner", "repo", &server.url())
            .await
            .unwrap_err();
        assert!(err.contains("not installed"), "got: {err}");
        assert!(err.contains("owner"), "got: {err}");
    }

    #[tokio::test]
    async fn create_iat_suspended() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(403)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let err = create_installation_access_token(&client, "jwt", "owner", "repo", &server.url())
            .await
            .unwrap_err();
        assert!(err.contains("suspended"), "got: {err}");
    }

    #[tokio::test]
    async fn create_iat_no_repo_access() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 123}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/123/access_tokens")
            .with_status(422)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let err = create_installation_access_token(&client, "jwt", "owner", "repo", &server.url())
            .await
            .unwrap_err();
        assert!(err.contains("does not have access"), "got: {err}");
        assert!(err.contains("repo"), "got: {err}");
    }

    #[tokio::test]
    async fn create_iat_auth_failed() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(401)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let err = create_installation_access_token(&client, "jwt", "owner", "repo", &server.url())
            .await
            .unwrap_err();
        assert!(err.contains("authentication failed"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // create_installation_access_token_for_pr
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_iat_for_pr_requests_pr_permissions() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/repos/owner/repo/installation")
            .match_header("Authorization", "Bearer test-jwt")
            .with_status(200)
            .with_body(r#"{"id": 456}"#)
            .create_async()
            .await;

        let token_mock = server
            .mock("POST", "/app/installations/456/access_tokens")
            .match_header("Authorization", "Bearer test-jwt")
            .match_body(mockito::Matcher::JsonString(
                r#"{"repositories":["repo"],"permissions":{"contents":"write","pull_requests":"write"}}"#.to_string(),
            ))
            .with_status(201)
            .with_body(r#"{"token": "ghs_pr_token"}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let token = create_installation_access_token_for_pr(
            &client,
            "test-jwt",
            "owner",
            "repo",
            &server.url(),
        )
        .await
        .unwrap();
        assert_eq!(token, "ghs_pr_token");

        token_mock.assert_async().await;
    }

    // -----------------------------------------------------------------------
    // branch_exists
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn branch_exists_returns_true_on_200() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("GET", "/repos/owner/repo/branches/my-branch")
            .with_status(200)
            .with_body(r#"{"name": "my-branch"}"#)
            .create_async()
            .await;

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem,
        };
        let result = branch_exists(&creds, "owner", "repo", "my-branch", &server.url()).await;
        assert_eq!(result.unwrap(), true);
    }

    #[tokio::test]
    async fn branch_exists_returns_false_on_404() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("GET", "/repos/owner/repo/branches/no-such-branch")
            .with_status(404)
            .create_async()
            .await;

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem,
        };
        let result = branch_exists(&creds, "owner", "repo", "no-such-branch", &server.url()).await;
        assert_eq!(result.unwrap(), false);
    }

    #[tokio::test]
    async fn branch_exists_returns_error_on_500() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("GET", "/repos/owner/repo/branches/broken")
            .with_status(500)
            .create_async()
            .await;

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem,
        };
        let result = branch_exists(&creds, "owner", "repo", "broken", &server.url()).await;
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // check_app_installed
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn check_app_installed_returns_true_on_200() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/repos/owner/repo/installation")
            .match_header("Authorization", "Bearer test-jwt")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let result = check_app_installed(&client, "test-jwt", "owner", "repo", &server.url()).await;
        assert_eq!(result.unwrap(), true);
    }

    #[tokio::test]
    async fn check_app_installed_returns_false_on_404() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(404)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let result = check_app_installed(&client, "test-jwt", "owner", "repo", &server.url()).await;
        assert_eq!(result.unwrap(), false);
    }

    #[tokio::test]
    async fn check_app_installed_returns_error_on_401() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(401)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let result = check_app_installed(&client, "test-jwt", "owner", "repo", &server.url()).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("authentication failed"),
            "expected auth error"
        );
    }

    // -----------------------------------------------------------------------
    // get_pull_request
    // -----------------------------------------------------------------------

    fn mock_pr_json() -> String {
        r#"{
            "number": 42,
            "title": "Fix the bug",
            "body": "Detailed description",
            "state": "open",
            "draft": false,
            "mergeable": true,
            "additions": 10,
            "deletions": 3,
            "changed_files": 2,
            "html_url": "https://github.com/owner/repo/pull/42",
            "user": {"login": "testuser"},
            "head": {"ref": "feature-branch"},
            "base": {"ref": "main"},
            "created_at": "2026-01-01T12:00:00Z",
            "updated_at": "2026-01-02T12:00:00Z"
        }"#
        .to_string()
    }

    #[tokio::test]
    async fn get_pr_success() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("GET", "/repos/owner/repo/pulls/42")
            .with_status(200)
            .with_body(mock_pr_json())
            .create_async()
            .await;

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem,
        };
        let detail = get_pull_request(&creds, "owner", "repo", 42, &server.url())
            .await
            .unwrap();

        assert_eq!(detail.number, 42);
        assert_eq!(detail.title, "Fix the bug");
        assert_eq!(detail.state, "open");
        assert_eq!(detail.additions, 10);
        assert_eq!(detail.deletions, 3);
        assert_eq!(detail.changed_files, 2);
        assert_eq!(detail.user.login, "testuser");
        assert_eq!(detail.head.ref_name, "feature-branch");
        assert_eq!(detail.base.ref_name, "main");
    }

    #[tokio::test]
    async fn get_pr_not_found() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("GET", "/repos/owner/repo/pulls/999")
            .with_status(404)
            .create_async()
            .await;

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem,
        };
        let err = get_pull_request(&creds, "owner", "repo", 999, &server.url())
            .await
            .unwrap_err();
        assert!(err.contains("not found"), "got: {err}");
        assert!(err.contains("#999"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // merge_pull_request
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn merge_pr_success() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("PUT", "/repos/owner/repo/pulls/42/merge")
            .with_status(200)
            .with_body(r#"{"merged": true}"#)
            .create_async()
            .await;

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem,
        };
        merge_pull_request(&creds, "owner", "repo", 42, "squash", &server.url())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn merge_pr_not_mergeable() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("PUT", "/repos/owner/repo/pulls/42/merge")
            .with_status(405)
            .create_async()
            .await;

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem,
        };
        let err = merge_pull_request(&creds, "owner", "repo", 42, "squash", &server.url())
            .await
            .unwrap_err();
        assert!(err.contains("not mergeable"), "got: {err}");
    }

    #[tokio::test]
    async fn merge_pr_conflict() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("PUT", "/repos/owner/repo/pulls/42/merge")
            .with_status(409)
            .create_async()
            .await;

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem,
        };
        let err = merge_pull_request(&creds, "owner", "repo", 42, "squash", &server.url())
            .await
            .unwrap_err();
        assert!(err.contains("merge conflict"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // close_pull_request
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn close_pr_success() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("PATCH", "/repos/owner/repo/pulls/42")
            .with_status(200)
            .with_body(mock_pr_json())
            .create_async()
            .await;

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem,
        };
        close_pull_request(&creds, "owner", "repo", 42, &server.url())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn close_pr_not_found() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("PATCH", "/repos/owner/repo/pulls/999")
            .with_status(404)
            .create_async()
            .await;

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem,
        };
        let err = close_pull_request(&creds, "owner", "repo", 999, &server.url())
            .await
            .unwrap_err();
        assert!(err.contains("not found"), "got: {err}");
        assert!(err.contains("#999"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // execute_github_graphql
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn execute_github_graphql_success() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/graphql")
            .match_header("Authorization", "Bearer test-token")
            .match_header("Content-Type", "application/json")
            .match_header("User-Agent", "fabro")
            .with_status(200)
            .with_body(r#"{"data": {"viewer": {"id": "U_abc"}}}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let result = execute_github_graphql(
            &client,
            "test-token",
            &format!("{}/graphql", server.url()),
            "query { viewer { id } }",
            serde_json::json!({}),
        )
        .await
        .unwrap();

        assert_eq!(result["data"]["viewer"]["id"], "U_abc");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn execute_github_graphql_http_error() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("POST", "/graphql")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let err = execute_github_graphql(
            &client,
            "token",
            &format!("{}/graphql", server.url()),
            "query { viewer { id } }",
            serde_json::json!({}),
        )
        .await
        .unwrap_err();

        assert!(err.contains("500"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_github_graphql_graphql_errors() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"data": null, "errors": [{"message": "Field 'foo' doesn't exist"}]}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let err = execute_github_graphql(
            &client,
            "token",
            &format!("{}/graphql", server.url()),
            "query { foo }",
            serde_json::json!({}),
        )
        .await
        .unwrap_err();

        assert!(err.contains("Field 'foo' doesn't exist"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_github_graphql_correct_headers() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("POST", "/graphql")
            .match_header("Authorization", "Bearer my-token")
            .match_header("Content-Type", "application/json")
            .match_header("User-Agent", "fabro")
            .with_status(200)
            .with_body(r#"{"data": {}}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        execute_github_graphql(
            &client,
            "my-token",
            &format!("{}/graphql", server.url()),
            "query { viewer { id } }",
            serde_json::json!({}),
        )
        .await
        .unwrap();

        mock.assert_async().await;
    }

    // -----------------------------------------------------------------------
    // GitHubTracker helpers
    // -----------------------------------------------------------------------

    fn mock_github_tracker(server_url: &str, pem: String) -> GitHubTracker {
        GitHubTracker::new(
            GitHubAppCredentials {
                app_id: "test-app".to_string(),
                private_key_pem: pem,
            },
            reqwest::Client::new(),
            "owner".to_string(),
            "repo".to_string(),
            1,
            server_url.to_string(),
        )
    }

    fn make_test_issue(state: &str) -> Issue {
        Issue {
            id: "I_issue1".to_string(),
            project_item_id: Some("PVTI_item1".to_string()),
            identifier: "#42".to_string(),
            title: "Fix bug".to_string(),
            description: None,
            priority: None,
            state: state.to_string(),
            branch_name: None,
            url: "https://github.com/owner/repo/issues/42".to_string(),
            assignee_id: None,
            labels: vec![],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        }
    }

    fn org_project_node_id_response() -> &'static str {
        r#"{"data": {"organization": {"projectV2": {"id": "PVT_abc123"}}}}"#
    }

    fn empty_items_response() -> &'static str {
        r#"{"data": {"node": {"items": {"nodes": [], "pageInfo": {"hasNextPage": false, "endCursor": null}}}}}"#
    }

    fn single_item_response(status: &str) -> String {
        serde_json::json!({
            "data": {
                "node": {
                    "items": {
                        "nodes": [
                            {
                                "id": "PVTI_item1",
                                "fieldValueByName": {"name": status},
                                "content": {
                                    "id": "I_issue1",
                                    "number": 42,
                                    "title": "Fix bug",
                                    "body": "Description",
                                    "url": "https://github.com/owner/repo/issues/42",
                                    "createdAt": "2026-01-01T00:00:00Z",
                                    "updatedAt": "2026-01-02T00:00:00Z",
                                    "assignees": {"nodes": []},
                                    "labels": {"nodes": [{"name": "bug"}]}
                                }
                            }
                        ],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }
                }
            }
        })
        .to_string()
    }

    // -----------------------------------------------------------------------
    // project node ID resolution
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn project_node_id_resolved_via_org() {
        let mut server = mockito::Server::new_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(), pem);

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(org_project_node_id_response())
            .create_async()
            .await;
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(empty_items_response())
            .create_async()
            .await;

        let issues = tracker.fetch_candidate_issues(&["Todo"]).await.unwrap();
        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn project_node_id_falls_back_to_user() {
        let mut server = mockito::Server::new_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(), pem);

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        // Org query returns null → fall back to user
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"data": {"organization": null}}"#)
            .create_async()
            .await;
        // User query succeeds
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"data": {"user": {"projectV2": {"id": "PVT_user1"}}}}"#)
            .create_async()
            .await;
        // Items page (empty)
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(empty_items_response())
            .create_async()
            .await;

        let issues = tracker.fetch_candidate_issues(&["Todo"]).await.unwrap();
        assert!(issues.is_empty());
    }

    // -----------------------------------------------------------------------
    // fetch_viewer_id (GitHubTracker)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn github_tracker_fetch_viewer_id_success() {
        let mut server = mockito::Server::new_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(), pem);

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"data": {"viewer": {"id": "U_xyz"}}}"#)
            .create_async()
            .await;

        let id = tracker.fetch_viewer_id().await.unwrap();
        assert_eq!(id, "U_xyz");
    }

    // -----------------------------------------------------------------------
    // create_comment (GitHubTracker)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn github_tracker_create_comment_success() {
        let mut server = mockito::Server::new_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(), pem);

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"data": {"addComment": {"clientMutationId": null}}}"#)
            .create_async()
            .await;

        let issue = make_test_issue("In Progress");
        tracker.create_comment(&issue, "Great work!").await.unwrap();
    }

    // -----------------------------------------------------------------------
    // update_issue_state (GitHubTracker)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn github_tracker_update_issue_state_success() {
        let mut server = mockito::Server::new_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(), pem);

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        // Resolve project node ID (org path)
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(org_project_node_id_response())
            .create_async()
            .await;
        // Field query
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"data": {"node": {"field": {"id": "FLD_1", "options": [{"id": "opt-done", "name": "Done"}, {"id": "opt-todo", "name": "Todo"}]}}}}"#)
            .create_async()
            .await;
        // Update mutation
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"data": {"updateProjectV2ItemFieldValue": {"projectV2Item": {"id": "PVTI_item1"}}}}"#)
            .create_async()
            .await;

        let issue = make_test_issue("In Progress");
        tracker.update_issue_state(&issue, "Done").await.unwrap();
    }

    #[tokio::test]
    async fn github_tracker_update_issue_state_status_not_found() {
        let mut server = mockito::Server::new_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(), pem);

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        // Resolve project node ID
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(org_project_node_id_response())
            .create_async()
            .await;
        // Field query — options don't include "Nonexistent"
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"data": {"node": {"field": {"id": "FLD_1", "options": [{"id": "opt-done", "name": "Done"}]}}}}"#)
            .create_async()
            .await;

        let issue = make_test_issue("Todo");
        let err = tracker
            .update_issue_state(&issue, "Nonexistent")
            .await
            .unwrap_err();
        assert!(err.contains("Nonexistent"), "got: {err}");
        assert!(err.contains("not found"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // fetch_candidate_issues (GitHubTracker)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn github_tracker_fetch_candidate_issues_single_page() {
        let mut server = mockito::Server::new_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(), pem);

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(org_project_node_id_response())
            .create_async()
            .await;
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(single_item_response("In Progress"))
            .create_async()
            .await;

        let issues = tracker
            .fetch_candidate_issues(&["In Progress"])
            .await
            .unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].identifier, "#42");
        assert_eq!(issues[0].state, "In Progress");
        assert_eq!(issues[0].id, "I_issue1");
        assert_eq!(issues[0].project_item_id.as_deref(), Some("PVTI_item1"));
        assert_eq!(issues[0].labels, vec!["bug"]);
        assert!(issues[0].branch_name.is_none());
        assert!(issues[0].priority.is_none());
    }

    #[tokio::test]
    async fn github_tracker_fetch_candidate_issues_empty() {
        let mut server = mockito::Server::new_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(), pem);

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(org_project_node_id_response())
            .create_async()
            .await;
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(empty_items_response())
            .create_async()
            .await;

        let issues = tracker.fetch_candidate_issues(&["Todo"]).await.unwrap();
        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn github_tracker_fetch_candidate_issues_status_filtering() {
        let mut server = mockito::Server::new_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(), pem);

        let items_body = serde_json::json!({
            "data": {
                "node": {
                    "items": {
                        "nodes": [
                            {
                                "id": "PVTI_done",
                                "fieldValueByName": {"name": "Done"},
                                "content": {
                                    "id": "I_done1", "number": 10, "title": "Done issue",
                                    "body": null, "url": "https://github.com/owner/repo/issues/10",
                                    "createdAt": null, "updatedAt": null,
                                    "assignees": {"nodes": []}, "labels": {"nodes": []}
                                }
                            },
                            {
                                "id": "PVTI_inprog",
                                "fieldValueByName": {"name": "In Progress"},
                                "content": {
                                    "id": "I_inprog1", "number": 20, "title": "Active issue",
                                    "body": null, "url": "https://github.com/owner/repo/issues/20",
                                    "createdAt": null, "updatedAt": null,
                                    "assignees": {"nodes": []}, "labels": {"nodes": []}
                                }
                            }
                        ],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }
                }
            }
        })
        .to_string();

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(org_project_node_id_response())
            .create_async()
            .await;
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(items_body)
            .create_async()
            .await;

        let issues = tracker
            .fetch_candidate_issues(&["In Progress"])
            .await
            .unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].identifier, "#20");
    }

    // -----------------------------------------------------------------------
    // fetch_issues_by_ids (GitHubTracker)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn github_tracker_fetch_issues_by_ids_ordering() {
        let mut server = mockito::Server::new_async().await;
        let pem = test_rsa_key();
        let tracker = mock_github_tracker(&server.url(), pem);

        // Page returns issues in reverse order of what we request
        let items_body = serde_json::json!({
            "data": {
                "node": {
                    "items": {
                        "nodes": [
                            {
                                "id": "PVTI_b",
                                "fieldValueByName": {"name": "Todo"},
                                "content": {
                                    "id": "I_b", "number": 2, "title": "B",
                                    "body": null, "url": "https://github.com/owner/repo/issues/2",
                                    "createdAt": null, "updatedAt": null,
                                    "assignees": {"nodes": []}, "labels": {"nodes": []}
                                }
                            },
                            {
                                "id": "PVTI_a",
                                "fieldValueByName": {"name": "Todo"},
                                "content": {
                                    "id": "I_a", "number": 1, "title": "A",
                                    "body": null, "url": "https://github.com/owner/repo/issues/1",
                                    "createdAt": null, "updatedAt": null,
                                    "assignees": {"nodes": []}, "labels": {"nodes": []}
                                }
                            }
                        ],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}
                    }
                }
            }
        })
        .to_string();

        server
            .mock("GET", "/repos/owner/repo/installation")
            .with_status(200)
            .with_body(r#"{"id": 1}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/app/installations/1/access_tokens")
            .with_status(201)
            .with_body(r#"{"token": "ghs_test"}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(org_project_node_id_response())
            .create_async()
            .await;
        server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(items_body)
            .create_async()
            .await;

        // Request in A, B order — should get back in A, B order despite page returning B, A
        let issues = tracker.fetch_issues_by_ids(&["I_a", "I_b"]).await.unwrap();

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].id, "I_a");
        assert_eq!(issues[1].id, "I_b");
    }

    #[tokio::test]
    async fn github_tracker_fetch_issues_by_ids_empty() {
        let pem = test_rsa_key();
        let tracker = mock_github_tracker("http://unused", pem);

        // Empty input → no HTTP calls at all
        let issues = tracker.fetch_issues_by_ids(&[]).await.unwrap();
        assert!(issues.is_empty());
    }
}
