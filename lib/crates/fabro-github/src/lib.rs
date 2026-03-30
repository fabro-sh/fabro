use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;

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

/// Owner information for a GitHub App.
#[derive(Debug, Clone, Deserialize)]
pub struct AppOwner {
    pub login: String,
}

/// Information about a GitHub App from the authenticated `/app` endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct AppInfo {
    pub slug: String,
    pub owner: AppOwner,
}

/// Credentials for authenticating as a GitHub App.
#[derive(Clone, Debug)]
pub struct GitHubAppCredentials {
    pub app_id: String,
    pub private_key_pem: String,
}

impl GitHubAppCredentials {
    pub fn private_key_from_env() -> Result<Option<String>, String> {
        let Ok(raw) = std::env::var("GITHUB_APP_PRIVATE_KEY") else {
            return Ok(None);
        };
        decode_pem_env("GITHUB_APP_PRIVATE_KEY", &raw).map(Some)
    }

    pub fn from_env(app_id: Option<&str>) -> Result<Option<Self>, String> {
        let Some(app_id) = app_id else {
            return Ok(None);
        };
        let Some(private_key_pem) = Self::private_key_from_env()? else {
            return Ok(None);
        };
        Ok(Some(Self {
            app_id: app_id.to_string(),
            private_key_pem,
        }))
    }
}

fn decode_pem_env(name: &str, raw: &str) -> Result<String, String> {
    if raw.starts_with("-----") {
        return Ok(raw.to_string());
    }
    let pem_bytes = STANDARD
        .decode(raw)
        .map_err(|err| format!("{name} is not valid PEM or base64: {err}"))?;
    String::from_utf8(pem_bytes)
        .map_err(|err| format!("{name} base64 decoded to invalid UTF-8: {err}"))
}

/// HTTP method used in GitHub API calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
}

/// A minimal HTTP response for testability.
pub struct HttpResponse {
    pub status: u16,
    body: String,
}

impl HttpResponse {
    pub fn new(status: u16, body: String) -> Self {
        Self { status, body }
    }

    pub fn json<T: for<'de> Deserialize<'de>>(&self) -> Result<T, String> {
        serde_json::from_str(&self.body).map_err(|e| format!("Failed to parse response: {e}"))
    }

    pub fn text(&self) -> &str {
        &self.body
    }
}

/// Abstract HTTP client for GitHub API calls.
///
/// Implemented for `reqwest::Client` in production; tests use a mock
/// to avoid TCP/process overhead.
pub trait HttpClient: Send + Sync {
    fn request(
        &self,
        method: HttpMethod,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> impl std::future::Future<Output = Result<HttpResponse, String>> + Send;
}

impl HttpClient for reqwest::Client {
    async fn request(
        &self,
        method: HttpMethod,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> Result<HttpResponse, String> {
        let mut builder = match method {
            HttpMethod::Get => self.get(url),
            HttpMethod::Post => self.post(url),
            HttpMethod::Put => self.put(url),
            HttpMethod::Patch => self.patch(url),
        };
        for &(key, value) in headers {
            builder = builder.header(key, value);
        }
        if let Some(json_body) = body {
            builder = builder.json(json_body);
        }
        let resp = builder.send().await.map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        Ok(HttpResponse::new(status, text))
    }
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
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
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

/// Standard GitHub API headers for authenticated requests.
fn github_headers(auth: &str) -> [(&str, &str); 3] {
    [
        ("Authorization", auth),
        ("Accept", "application/vnd.github+json"),
        ("User-Agent", "fabro"),
    ]
}

/// Request a scoped Installation Access Token for a specific repository.
///
/// Uses the App JWT to find the installation for `owner/repo`, then requests
/// a token scoped to the given `permissions` on that single repository.
pub async fn create_installation_access_token_with_permissions(
    client: &impl HttpClient,
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
    let auth = format!("Bearer {jwt}");
    let resp = client
        .request(HttpMethod::Get, &install_url, &github_headers(&auth), None)
        .await
        .map_err(|e| format!("Failed to look up GitHub App installation: {e}"))?;

    match resp.status {
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
                "Unexpected status {} looking up GitHub App installation",
                resp.status
            ));
        }
    }

    let installation: Installation = resp
        .json()
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
        .request(
            HttpMethod::Post,
            &token_url,
            &github_headers(&auth),
            Some(&body),
        )
        .await
        .map_err(|e| format!("Failed to create installation access token: {e}"))?;

    match token_resp.status {
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
                "Unexpected status {} creating installation access token",
                token_resp.status
            ));
        }
    }

    let access_token: AccessToken = token_resp
        .json()
        .map_err(|e| format!("Failed to parse access token response: {e}"))?;

    Ok(access_token.token)
}

/// Request a scoped Installation Access Token with `contents: write`.
pub async fn create_installation_access_token(
    client: &impl HttpClient,
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
    client: &impl HttpClient,
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
    #[derive(Deserialize)]
    struct PullRequestResponse {
        html_url: String,
        number: u64,
        node_id: String,
    }

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
    let auth = format!("Bearer {token}");
    let resp = HttpClient::request(
        &client,
        HttpMethod::Post,
        &url,
        &github_headers(&auth),
        Some(&pr_body),
    )
    .await
    .map_err(|e| format!("Failed to create pull request: {e}"))?;

    match resp.status {
        201 => {}
        422 => {
            return Err(format!(
                "Pull request could not be created (422): {}",
                resp.text()
            ));
        }
        401 | 403 => {
            return Err(format!(
                "Authentication failed creating pull request ({})",
                resp.status
            ));
        }
        _ => {
            return Err(format!(
                "Unexpected status {} creating pull request: {}",
                resp.status,
                resp.text()
            ));
        }
    }

    let pr: PullRequestResponse = resp
        .json()
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

    let graphql_url = format!("{GITHUB_API_BASE_URL}/graphql");
    let auth = format!("Bearer {token}");
    let graphql_body = serde_json::json!({ "query": query });
    let resp = HttpClient::request(
        &client,
        HttpMethod::Post,
        &graphql_url,
        &[("Authorization", auth.as_str()), ("User-Agent", "fabro")],
        Some(&graphql_body),
    )
    .await
    .map_err(|e| format!("Failed to enable auto-merge: {e}"))?;

    let status = resp.status;
    let body: serde_json::Value = resp
        .json()
        .map_err(|e| format!("Failed to parse auto-merge response: {e}"))?;

    if !(200..300).contains(&status) {
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
    branch_exists_with_client(
        &reqwest::Client::new(),
        creds,
        owner,
        repo,
        branch,
        base_url,
    )
    .await
}

async fn branch_exists_with_client(
    client: &impl HttpClient,
    creds: &GitHubAppCredentials,
    owner: &str,
    repo: &str,
    branch: &str,
    base_url: &str,
) -> Result<bool, String> {
    let jwt = sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
    let token = create_installation_access_token(client, &jwt, owner, repo, base_url).await?;

    let url = format!("{base_url}/repos/{owner}/{repo}/branches/{branch}");
    let auth = format!("Bearer {token}");
    let resp = client
        .request(HttpMethod::Get, &url, &github_headers(&auth), None)
        .await
        .map_err(|e| format!("Failed to check branch existence: {e}"))?;

    match resp.status {
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
    client: &impl HttpClient,
    jwt: &str,
    owner: &str,
    repo: &str,
    base_url: &str,
) -> Result<bool, String> {
    let url = format!("{base_url}/repos/{owner}/{repo}/installation");
    let auth = format!("Bearer {jwt}");
    let resp = client
        .request(HttpMethod::Get, &url, &github_headers(&auth), None)
        .await
        .map_err(|e| format!("Failed to check GitHub App installation: {e}"))?;

    match resp.status {
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

/// Fetch information about the authenticated GitHub App.
///
/// Uses the App JWT to call `GET /app` and returns the app's slug and owner.
pub async fn get_authenticated_app(
    client: &impl HttpClient,
    jwt: &str,
    base_url: &str,
) -> Result<AppInfo, String> {
    let url = format!("{base_url}/app");
    let auth = format!("Bearer {jwt}");
    let resp = client
        .request(HttpMethod::Get, &url, &github_headers(&auth), None)
        .await
        .map_err(|e| format!("Failed to fetch GitHub App info: {e}"))?;

    match resp.status {
        200 => {}
        401 => {
            return Err("GitHub App authentication failed. \
                 Check that app_id and GITHUB_APP_PRIVATE_KEY are correct."
                .to_string());
        }
        status => {
            return Err(format!(
                "Unexpected status {status} fetching GitHub App info"
            ));
        }
    }

    resp.json::<AppInfo>()
        .map_err(|e| format!("Failed to parse GitHub App info: {e}"))
}

/// Check whether a GitHub App is publicly visible.
///
/// Calls `GET /apps/{slug}` **without** authentication. Public apps return 200,
/// private apps return 404 to unauthenticated requests.
pub async fn is_app_public(
    client: &impl HttpClient,
    slug: &str,
    base_url: &str,
) -> Result<bool, String> {
    let url = format!("{base_url}/apps/{slug}");
    let resp = client
        .request(
            HttpMethod::Get,
            &url,
            &[
                ("Accept", "application/vnd.github+json"),
                ("User-Agent", "fabro"),
            ],
            None,
        )
        .await
        .map_err(|e| format!("Failed to check GitHub App visibility: {e}"))?;

    match resp.status {
        200 => Ok(true),
        404 => Ok(false),
        status => Err(format!(
            "Unexpected status {status} checking GitHub App visibility"
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
    get_pull_request_with_client(
        &reqwest::Client::new(),
        creds,
        owner,
        repo,
        number,
        base_url,
    )
    .await
}

async fn get_pull_request_with_client(
    client: &impl HttpClient,
    creds: &GitHubAppCredentials,
    owner: &str,
    repo: &str,
    number: u64,
    base_url: &str,
) -> Result<PullRequestDetail, String> {
    tracing::debug!(owner, repo, number, "Fetching pull request");

    let jwt = sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
    let token =
        create_installation_access_token_for_pr(client, &jwt, owner, repo, base_url).await?;

    let url = format!("{base_url}/repos/{owner}/{repo}/pulls/{number}");
    let auth = format!("Bearer {token}");
    let resp = client
        .request(HttpMethod::Get, &url, &github_headers(&auth), None)
        .await
        .map_err(|e| format!("Failed to fetch pull request: {e}"))?;

    match resp.status {
        200 => {}
        404 => {
            return Err(format!(
                "Pull request #{number} not found in {owner}/{repo}"
            ));
        }
        401 | 403 => {
            return Err(format!(
                "Authentication failed fetching pull request ({})",
                resp.status
            ));
        }
        status => {
            return Err(format!(
                "Unexpected status {status} fetching pull request: {}",
                resp.text()
            ));
        }
    }

    resp.json::<PullRequestDetail>()
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
    merge_pull_request_with_client(
        &reqwest::Client::new(),
        creds,
        owner,
        repo,
        number,
        method,
        base_url,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn merge_pull_request_with_client(
    client: &impl HttpClient,
    creds: &GitHubAppCredentials,
    owner: &str,
    repo: &str,
    number: u64,
    method: &str,
    base_url: &str,
) -> Result<(), String> {
    tracing::debug!(owner, repo, number, method, "Merging pull request");

    let jwt = sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
    let token =
        create_installation_access_token_for_pr(client, &jwt, owner, repo, base_url).await?;

    let url = format!("{base_url}/repos/{owner}/{repo}/pulls/{number}/merge");
    let body = serde_json::json!({ "merge_method": method });
    let auth = format!("Bearer {token}");

    let resp = client
        .request(HttpMethod::Put, &url, &github_headers(&auth), Some(&body))
        .await
        .map_err(|e| format!("Failed to merge pull request: {e}"))?;

    match resp.status {
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
            resp.status
        )),
        status => Err(format!(
            "Unexpected status {status} merging pull request: {}",
            resp.text()
        )),
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
    close_pull_request_with_client(
        &reqwest::Client::new(),
        creds,
        owner,
        repo,
        number,
        base_url,
    )
    .await
}

async fn close_pull_request_with_client(
    client: &impl HttpClient,
    creds: &GitHubAppCredentials,
    owner: &str,
    repo: &str,
    number: u64,
    base_url: &str,
) -> Result<(), String> {
    tracing::debug!(owner, repo, number, "Closing pull request");

    let jwt = sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
    let token =
        create_installation_access_token_for_pr(client, &jwt, owner, repo, base_url).await?;

    let url = format!("{base_url}/repos/{owner}/{repo}/pulls/{number}");
    let body = serde_json::json!({ "state": "closed" });
    let auth = format!("Bearer {token}");

    let resp = client
        .request(HttpMethod::Patch, &url, &github_headers(&auth), Some(&body))
        .await
        .map_err(|e| format!("Failed to close pull request: {e}"))?;

    match resp.status {
        200 => Ok(()),
        404 => Err(format!(
            "Pull request #{number} not found in {owner}/{repo}"
        )),
        401 | 403 => Err(format!(
            "Authentication failed closing pull request ({})",
            resp.status
        )),
        status => Err(format!(
            "Unexpected status {status} closing pull request: {}",
            resp.text()
        )),
    }
}

/// Request a scoped Installation Access Token with `issues: write`
/// and `organization_projects: write`. Used for GitHub Projects V2.
pub async fn create_installation_access_token_for_projects(
    client: &impl HttpClient,
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    #[test]
    fn decode_pem_env_accepts_raw_pem() {
        let pem = "-----BEGIN TEST KEY-----\nabc\n-----END TEST KEY-----";
        assert_eq!(decode_pem_env("GITHUB_APP_PRIVATE_KEY", pem).unwrap(), pem);
    }

    #[test]
    fn decode_pem_env_accepts_base64_pem() {
        let pem = "-----BEGIN TEST KEY-----\nabc\n-----END TEST KEY-----";
        let encoded = STANDARD.encode(pem);
        assert_eq!(
            decode_pem_env("GITHUB_APP_PRIVATE_KEY", &encoded).unwrap(),
            pem
        );
    }

    #[test]
    fn decode_pem_env_rejects_invalid_base64() {
        let err = decode_pem_env("GITHUB_APP_PRIVATE_KEY", "%%%not-base64%%%").unwrap_err();
        assert!(err.contains("GITHUB_APP_PRIVATE_KEY is not valid PEM or base64"));
    }

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

    fn test_rsa_key() -> &'static str {
        include_str!("testdata/rsa_private.pem")
    }

    #[test]
    fn jwt_is_three_part_string() {
        let pem = test_rsa_key();
        let jwt = sign_app_jwt("12345", pem).unwrap();
        assert_eq!(jwt.split('.').count(), 3);
    }

    #[test]
    fn jwt_has_rs256_header() {
        let pem = test_rsa_key();
        let jwt = sign_app_jwt("12345", pem).unwrap();
        let header_b64 = jwt.split('.').next().unwrap();
        let header_json = URL_SAFE_NO_PAD.decode(header_b64).unwrap();
        let header: serde_json::Value = serde_json::from_slice(&header_json).unwrap();
        assert_eq!(header["alg"], "RS256");
    }

    #[test]
    fn jwt_has_correct_claims() {
        let pem = test_rsa_key();
        let jwt = sign_app_jwt("99999", pem).unwrap();
        let payload_b64 = jwt.split('.').nth(1).unwrap();
        let payload_json = URL_SAFE_NO_PAD.decode(payload_b64).unwrap();
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
    // MockHttpClient
    // -----------------------------------------------------------------------

    struct MockRoute {
        method: HttpMethod,
        path: String,
        status: u16,
        response_body: String,
        assert_header: Option<(String, MockHeaderCheck)>,
        assert_body_json: Option<serde_json::Value>,
    }

    enum MockHeaderCheck {
        Equals(String),
        Missing,
    }

    struct MockHttpClient {
        routes: Vec<MockRoute>,
    }

    impl MockHttpClient {
        fn new() -> Self {
            Self { routes: vec![] }
        }

        fn on(mut self, method: HttpMethod, path: &str, status: u16, body: &str) -> Self {
            self.routes.push(MockRoute {
                method,
                path: path.to_string(),
                status,
                response_body: body.to_string(),
                assert_header: None,
                assert_body_json: None,
            });
            self
        }

        fn with_req_header(mut self, name: &str, value: &str) -> Self {
            self.routes.last_mut().unwrap().assert_header =
                Some((name.to_string(), MockHeaderCheck::Equals(value.to_string())));
            self
        }

        fn with_req_header_missing(mut self, name: &str) -> Self {
            self.routes.last_mut().unwrap().assert_header =
                Some((name.to_string(), MockHeaderCheck::Missing));
            self
        }

        fn with_req_body(mut self, json_str: &str) -> Self {
            self.routes.last_mut().unwrap().assert_body_json =
                Some(serde_json::from_str(json_str).unwrap());
            self
        }
    }

    impl HttpClient for MockHttpClient {
        async fn request(
            &self,
            method: HttpMethod,
            url: &str,
            headers: &[(&str, &str)],
            body: Option<&serde_json::Value>,
        ) -> Result<HttpResponse, String> {
            for route in &self.routes {
                if method == route.method && url.ends_with(&route.path) {
                    if let Some((name, check)) = &route.assert_header {
                        let found = headers.iter().find(|(k, _)| *k == name.as_str());
                        match check {
                            MockHeaderCheck::Equals(expected) => {
                                let (_, v) = found.unwrap_or_else(|| {
                                    panic!("Expected header '{name}' not found in request to {url}")
                                });
                                assert_eq!(
                                    *v,
                                    expected.as_str(),
                                    "Header '{name}' mismatch for {url}"
                                );
                            }
                            MockHeaderCheck::Missing => {
                                assert!(
                                    found.is_none(),
                                    "Header '{name}' should be absent for {url}"
                                );
                            }
                        }
                    }
                    if let Some(expected_body) = &route.assert_body_json {
                        let actual = body.expect("Expected request body");
                        assert_eq!(actual, expected_body, "Request body mismatch for {url}");
                    }
                    return Ok(HttpResponse::new(route.status, route.response_body.clone()));
                }
            }
            panic!(
                "No mock route for {:?} {url}\nRegistered routes: {:?}",
                method,
                self.routes
                    .iter()
                    .map(|r| format!("{:?} {}", r.method, r.path))
                    .collect::<Vec<_>>()
            );
        }
    }

    // -----------------------------------------------------------------------
    // create_installation_access_token — success
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_iat_success() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/installation",
                200,
                r#"{"id": 123}"#,
            )
            .with_req_header("Authorization", "Bearer test-jwt")
            .on(
                HttpMethod::Post,
                "/app/installations/123/access_tokens",
                201,
                r#"{"token": "ghs_xxx"}"#,
            )
            .with_req_header("Authorization", "Bearer test-jwt")
            .with_req_body(r#"{"permissions":{"contents":"write"},"repositories":["repo"]}"#);

        let token = create_installation_access_token(&mock, "test-jwt", "owner", "repo", "")
            .await
            .unwrap();
        assert_eq!(token, "ghs_xxx");
    }

    // -----------------------------------------------------------------------
    // create_installation_access_token — failure modes
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_iat_not_installed() {
        let mock =
            MockHttpClient::new().on(HttpMethod::Get, "/repos/owner/repo/installation", 404, "");

        let err = create_installation_access_token(&mock, "jwt", "owner", "repo", "")
            .await
            .unwrap_err();
        assert!(err.contains("not installed"), "got: {err}");
        assert!(err.contains("owner"), "got: {err}");
    }

    #[tokio::test]
    async fn create_iat_suspended() {
        let mock =
            MockHttpClient::new().on(HttpMethod::Get, "/repos/owner/repo/installation", 403, "");

        let err = create_installation_access_token(&mock, "jwt", "owner", "repo", "")
            .await
            .unwrap_err();
        assert!(err.contains("suspended"), "got: {err}");
    }

    #[tokio::test]
    async fn create_iat_no_repo_access() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/installation",
                200,
                r#"{"id": 123}"#,
            )
            .on(
                HttpMethod::Post,
                "/app/installations/123/access_tokens",
                422,
                "",
            );

        let err = create_installation_access_token(&mock, "jwt", "owner", "repo", "")
            .await
            .unwrap_err();
        assert!(err.contains("does not have access"), "got: {err}");
        assert!(err.contains("repo"), "got: {err}");
    }

    #[tokio::test]
    async fn create_iat_auth_failed() {
        let mock =
            MockHttpClient::new().on(HttpMethod::Get, "/repos/owner/repo/installation", 401, "");

        let err = create_installation_access_token(&mock, "jwt", "owner", "repo", "")
            .await
            .unwrap_err();
        assert!(err.contains("authentication failed"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // create_installation_access_token_for_pr
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_iat_for_pr_requests_pr_permissions() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/installation",
                200,
                r#"{"id": 456}"#,
            )
            .with_req_header("Authorization", "Bearer test-jwt")
            .on(
                HttpMethod::Post,
                "/app/installations/456/access_tokens",
                201,
                r#"{"token": "ghs_pr_token"}"#,
            )
            .with_req_header("Authorization", "Bearer test-jwt")
            .with_req_body(
                r#"{"permissions":{"contents":"write","pull_requests":"write"},"repositories":["repo"]}"#,
            );

        let token = create_installation_access_token_for_pr(&mock, "test-jwt", "owner", "repo", "")
            .await
            .unwrap();
        assert_eq!(token, "ghs_pr_token");
    }

    // -----------------------------------------------------------------------
    // branch_exists
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn branch_exists_returns_true_on_200() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/installation",
                200,
                r#"{"id": 1}"#,
            )
            .on(
                HttpMethod::Post,
                "/app/installations/1/access_tokens",
                201,
                r#"{"token": "ghs_test"}"#,
            )
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/branches/my-branch",
                200,
                r#"{"name": "my-branch"}"#,
            );

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem.to_string(),
        };
        let result =
            branch_exists_with_client(&mock, &creds, "owner", "repo", "my-branch", "").await;
        assert!(result.unwrap());
    }

    #[tokio::test]
    async fn branch_exists_returns_false_on_404() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/installation",
                200,
                r#"{"id": 1}"#,
            )
            .on(
                HttpMethod::Post,
                "/app/installations/1/access_tokens",
                201,
                r#"{"token": "ghs_test"}"#,
            )
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/branches/no-such-branch",
                404,
                "",
            );

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem.to_string(),
        };
        let result =
            branch_exists_with_client(&mock, &creds, "owner", "repo", "no-such-branch", "").await;
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn branch_exists_returns_error_on_500() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/installation",
                200,
                r#"{"id": 1}"#,
            )
            .on(
                HttpMethod::Post,
                "/app/installations/1/access_tokens",
                201,
                r#"{"token": "ghs_test"}"#,
            )
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/branches/broken",
                500,
                "",
            );

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem.to_string(),
        };
        let result = branch_exists_with_client(&mock, &creds, "owner", "repo", "broken", "").await;
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // check_app_installed
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn check_app_installed_returns_true_on_200() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/installation",
                200,
                r#"{"id": 1}"#,
            )
            .with_req_header("Authorization", "Bearer test-jwt");

        let result = check_app_installed(&mock, "test-jwt", "owner", "repo", "").await;
        assert!(result.unwrap());
    }

    #[tokio::test]
    async fn check_app_installed_returns_false_on_404() {
        let mock =
            MockHttpClient::new().on(HttpMethod::Get, "/repos/owner/repo/installation", 404, "");

        let result = check_app_installed(&mock, "test-jwt", "owner", "repo", "").await;
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn check_app_installed_returns_error_on_401() {
        let mock =
            MockHttpClient::new().on(HttpMethod::Get, "/repos/owner/repo/installation", 401, "");

        let result = check_app_installed(&mock, "test-jwt", "owner", "repo", "").await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("authentication failed"),
            "expected auth error"
        );
    }

    // -----------------------------------------------------------------------
    // get_authenticated_app
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_authenticated_app_success() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/app",
                200,
                r#"{"slug": "my-fabro-app", "owner": {"login": "my-org"}}"#,
            )
            .with_req_header("Authorization", "Bearer test-jwt");

        let info = get_authenticated_app(&mock, "test-jwt", "").await.unwrap();
        assert_eq!(info.slug, "my-fabro-app");
        assert_eq!(info.owner.login, "my-org");
    }

    #[tokio::test]
    async fn get_authenticated_app_auth_failure() {
        let mock = MockHttpClient::new().on(HttpMethod::Get, "/app", 401, "");

        let result = get_authenticated_app(&mock, "bad-jwt", "").await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("authentication failed"),
            "expected auth error"
        );
    }

    // -----------------------------------------------------------------------
    // is_app_public
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn is_app_public_returns_true_on_200() {
        let mock = MockHttpClient::new().on(
            HttpMethod::Get,
            "/apps/my-fabro-app",
            200,
            r#"{"slug": "my-fabro-app"}"#,
        );

        let result = is_app_public(&mock, "my-fabro-app", "").await;
        assert!(result.unwrap());
    }

    #[tokio::test]
    async fn is_app_public_returns_false_on_404() {
        let mock = MockHttpClient::new().on(HttpMethod::Get, "/apps/my-private-app", 404, "");

        let result = is_app_public(&mock, "my-private-app", "").await;
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn is_app_public_no_auth_header() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/apps/my-app",
                200,
                r#"{"slug": "my-app"}"#,
            )
            .with_req_header_missing("Authorization");

        let result = is_app_public(&mock, "my-app", "").await;
        assert!(result.unwrap());
    }

    // -----------------------------------------------------------------------
    // get_pull_request
    // -----------------------------------------------------------------------

    fn mock_pr_json() -> &'static str {
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
    }

    #[tokio::test]
    async fn get_pr_success() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/installation",
                200,
                r#"{"id": 1}"#,
            )
            .on(
                HttpMethod::Post,
                "/app/installations/1/access_tokens",
                201,
                r#"{"token": "ghs_test"}"#,
            )
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/pulls/42",
                200,
                mock_pr_json(),
            );

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem.to_string(),
        };
        let detail = get_pull_request_with_client(&mock, &creds, "owner", "repo", 42, "")
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
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/installation",
                200,
                r#"{"id": 1}"#,
            )
            .on(
                HttpMethod::Post,
                "/app/installations/1/access_tokens",
                201,
                r#"{"token": "ghs_test"}"#,
            )
            .on(HttpMethod::Get, "/repos/owner/repo/pulls/999", 404, "");

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem.to_string(),
        };
        let err = get_pull_request_with_client(&mock, &creds, "owner", "repo", 999, "")
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
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/installation",
                200,
                r#"{"id": 1}"#,
            )
            .on(
                HttpMethod::Post,
                "/app/installations/1/access_tokens",
                201,
                r#"{"token": "ghs_test"}"#,
            )
            .on(
                HttpMethod::Put,
                "/repos/owner/repo/pulls/42/merge",
                200,
                r#"{"merged": true}"#,
            );

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem.to_string(),
        };
        merge_pull_request_with_client(&mock, &creds, "owner", "repo", 42, "squash", "")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn merge_pr_not_mergeable() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/installation",
                200,
                r#"{"id": 1}"#,
            )
            .on(
                HttpMethod::Post,
                "/app/installations/1/access_tokens",
                201,
                r#"{"token": "ghs_test"}"#,
            )
            .on(HttpMethod::Put, "/repos/owner/repo/pulls/42/merge", 405, "");

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem.to_string(),
        };
        let err = merge_pull_request_with_client(&mock, &creds, "owner", "repo", 42, "squash", "")
            .await
            .unwrap_err();
        assert!(err.contains("not mergeable"), "got: {err}");
    }

    #[tokio::test]
    async fn merge_pr_conflict() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/installation",
                200,
                r#"{"id": 1}"#,
            )
            .on(
                HttpMethod::Post,
                "/app/installations/1/access_tokens",
                201,
                r#"{"token": "ghs_test"}"#,
            )
            .on(HttpMethod::Put, "/repos/owner/repo/pulls/42/merge", 409, "");

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem.to_string(),
        };
        let err = merge_pull_request_with_client(&mock, &creds, "owner", "repo", 42, "squash", "")
            .await
            .unwrap_err();
        assert!(err.contains("merge conflict"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // close_pull_request
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn close_pr_success() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/installation",
                200,
                r#"{"id": 1}"#,
            )
            .on(
                HttpMethod::Post,
                "/app/installations/1/access_tokens",
                201,
                r#"{"token": "ghs_test"}"#,
            )
            .on(
                HttpMethod::Patch,
                "/repos/owner/repo/pulls/42",
                200,
                mock_pr_json(),
            );

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem.to_string(),
        };
        close_pull_request_with_client(&mock, &creds, "owner", "repo", 42, "")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn close_pr_not_found() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/installation",
                200,
                r#"{"id": 1}"#,
            )
            .on(
                HttpMethod::Post,
                "/app/installations/1/access_tokens",
                201,
                r#"{"token": "ghs_test"}"#,
            )
            .on(HttpMethod::Patch, "/repos/owner/repo/pulls/999", 404, "");

        let pem = test_rsa_key();
        let creds = GitHubAppCredentials {
            app_id: "test".to_string(),
            private_key_pem: pem.to_string(),
        };
        let err = close_pull_request_with_client(&mock, &creds, "owner", "repo", 999, "")
            .await
            .unwrap_err();
        assert!(err.contains("not found"), "got: {err}");
        assert!(err.contains("#999"), "got: {err}");
    }
}
