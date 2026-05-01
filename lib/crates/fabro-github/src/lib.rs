use anyhow::{Context as _, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fabro_redact::DisplaySafeUrl;
use fabro_static::EnvVars;
use fabro_types::PullRequestGithubDetail;
use fabro_types::settings::run::MergeStrategy;
use serde::Deserialize;
use tokio::process::Command;

pub const GITHUB_API_BASE_URL: &str = "https://api.github.com";

/// Returns the GitHub API base URL, allowing override via `GITHUB_BASE_URL` env
/// var.
#[expect(
    clippy::disallowed_methods,
    reason = "GitHub API client exposes a documented process-env base URL override."
)]
pub fn github_api_base_url() -> String {
    std::env::var(EnvVars::GITHUB_BASE_URL).unwrap_or_else(|_| GITHUB_API_BASE_URL.to_string())
}

/// Bundle of GitHub credentials and the API base URL, threaded through every
/// authenticated GitHub call. Lets call sites pass one parameter instead of
/// two, and keeps the auth/endpoint pair from drifting apart.
#[derive(Debug, Clone)]
pub struct GitHubContext<'a> {
    creds:       &'a GitHubCredentials,
    base_url:    &'a str,
    http_client: Option<fabro_http::HttpClient>,
}

impl<'a> GitHubContext<'a> {
    pub fn new(creds: &'a GitHubCredentials, base_url: &'a str) -> Self {
        Self {
            creds,
            base_url,
            http_client: None,
        }
    }

    pub fn with_http_client(
        creds: &'a GitHubCredentials,
        base_url: &'a str,
        http_client: fabro_http::HttpClient,
    ) -> Self {
        Self {
            creds,
            base_url,
            http_client: Some(http_client),
        }
    }

    fn http_client(&self) -> anyhow::Result<fabro_http::HttpClient> {
        self.http_client.clone().map_or_else(http_client, Ok)
    }
}

/// Errors returned by pull-request endpoints. Callers branch on `NotFound` to
/// distinguish a missing PR from any other failure.
#[derive(Debug, thiserror::Error)]
pub enum PullRequestApiError {
    #[error("Pull request #{number} not found in {owner}/{repo}")]
    NotFound {
        owner:  String,
        repo:   String,
        number: u64,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

fn http_client() -> anyhow::Result<fabro_http::HttpClient> {
    fabro_http::http_client().map_err(Into::into)
}

/// Owner information for a GitHub App.
#[derive(Debug, Clone, Deserialize)]
pub struct AppOwner {
    pub login: String,
}

/// Information about a GitHub App from the authenticated `/app` endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct AppInfo {
    pub slug:  String,
    pub owner: AppOwner,
}

/// Credentials for authenticating as a GitHub App.
#[derive(Clone, Debug)]
pub struct GitHubAppCredentials {
    pub app_id:          String,
    pub private_key_pem: String,
    pub slug:            Option<String>,
}

impl GitHubAppCredentials {
    #[expect(
        clippy::disallowed_methods,
        reason = "GitHub App credentials support a documented private-key env source."
    )]
    pub fn private_key_from_env() -> Result<Option<String>, String> {
        let Ok(raw) = std::env::var(EnvVars::GITHUB_APP_PRIVATE_KEY) else {
            return Ok(None);
        };
        decode_pem_env(EnvVars::GITHUB_APP_PRIVATE_KEY, &raw).map(Some)
    }

    pub fn from_env(app_id: Option<&str>) -> Result<Option<Self>, String> {
        Self::from_env_with_slug(app_id, None)
    }

    pub fn from_env_with_slug(
        app_id: Option<&str>,
        slug: Option<&str>,
    ) -> Result<Option<Self>, String> {
        let Some(app_id) = app_id else {
            return Ok(None);
        };
        let Some(private_key_pem) = Self::private_key_from_env()? else {
            return Ok(None);
        };
        Ok(Some(Self {
            app_id: app_id.to_string(),
            private_key_pem,
            slug: slug
                .map(str::trim)
                .filter(|slug| !slug.is_empty())
                .map(str::to_string),
        }))
    }

    pub fn installation_url(&self, owner: &str) -> Option<String> {
        self.slug.as_ref().map(|slug| {
            format!("https://github.com/organizations/{owner}/settings/apps/{slug}/installations")
        })
    }
}

#[derive(Clone, Debug)]
pub enum GitHubCredentials {
    App(GitHubAppCredentials),
    Token(String),
}

impl GitHubCredentials {
    pub fn from_env(app_id: Option<&str>) -> Result<Option<Self>, String> {
        Ok(GitHubAppCredentials::from_env(app_id)?.map(Self::App))
    }

    pub fn from_env_with_slug(
        app_id: Option<&str>,
        slug: Option<&str>,
    ) -> Result<Option<Self>, String> {
        Ok(GitHubAppCredentials::from_env_with_slug(app_id, slug)?.map(Self::App))
    }

    pub async fn resolve_bearer_token(
        &self,
        client: &impl HttpClient,
        owner: &str,
        repo: &str,
        base_url: &str,
        permissions: serde_json::Value,
    ) -> anyhow::Result<String> {
        match self {
            Self::App(creds) => {
                let jwt = sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
                let install_url = creds.installation_url(owner);
                create_installation_access_token_with_permissions_and_install_url(
                    client,
                    &jwt,
                    owner,
                    repo,
                    base_url,
                    permissions,
                    install_url.as_deref(),
                )
                .await
            }
            Self::Token(token) => Ok(token.clone()),
        }
    }
}

pub async fn gh_auth_token() -> anyhow::Result<String> {
    let output = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .await
        .context("Failed to run `gh auth token`")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message = if stderr.is_empty() {
            format!("`gh auth token` exited with status {}", output.status)
        } else {
            stderr
        };
        bail!("Failed to get GitHub CLI token: {message}");
    }

    let token =
        String::from_utf8(output.stdout).context("`gh auth token` returned invalid UTF-8")?;
    let token = token.trim().to_string();
    if token.is_empty() {
        bail!("`gh auth token` returned an empty token");
    }
    Ok(token)
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
    body:       String,
}

impl HttpResponse {
    pub fn new(status: u16, body: String) -> Self {
        Self { status, body }
    }

    pub fn json<T: for<'de> Deserialize<'de>>(&self) -> anyhow::Result<T> {
        serde_json::from_str(&self.body).context("Failed to parse response")
    }

    pub fn text(&self) -> &str {
        &self.body
    }
}

/// Abstract HTTP client for GitHub API calls.
///
/// Implemented for `fabro_http::HttpClient` in production; tests use a mock
/// to avoid TCP/process overhead.
pub trait HttpClient: Send + Sync {
    fn request(
        &self,
        method: HttpMethod,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> impl std::future::Future<Output = anyhow::Result<HttpResponse>> + Send;
}

impl HttpClient for fabro_http::HttpClient {
    async fn request(
        &self,
        method: HttpMethod,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> anyhow::Result<HttpResponse> {
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
        let resp = builder.send().await.map_err(anyhow::Error::new)?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(anyhow::Error::new)?;
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
pub fn parse_github_owner_repo(url: &str) -> anyhow::Result<(String, String)> {
    // Strip credentials from URLs like https://x-access-token:TOKEN@github.com/...
    let stripped = url.strip_prefix("https://").and_then(|rest| {
        rest.split_once('@')
            .map(|(_, after)| format!("https://{after}"))
    });
    let url = stripped.as_deref().unwrap_or(url);
    let display_url = redacted_url_for_error(url);
    let path = url
        .strip_prefix("https://github.com/")
        .ok_or_else(|| anyhow!("Not a GitHub HTTPS URL: {display_url}"))?;

    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);

    let mut parts = path.splitn(3, '/');
    let owner = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Missing owner in GitHub URL: {display_url}"))?;
    let repo = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Missing repo in GitHub URL: {display_url}"))?;

    Ok((owner.to_string(), repo.to_string()))
}

fn redacted_url_for_error(url: &str) -> String {
    DisplaySafeUrl::parse(url)
        .map_or_else(|_| "<invalid url>".to_string(), |url| url.redacted_string())
}

/// Create a signed JWT for GitHub App authentication (RS256).
///
/// The JWT is valid for 10 minutes with a 60-second clock skew allowance.
pub fn sign_app_jwt(app_id: &str, private_key_pem: &str) -> anyhow::Result<String> {
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

    let key =
        EncodingKey::from_rsa_pem(private_key_pem.as_bytes()).context("Invalid RSA private key")?;

    let jwt =
        encode(&Header::new(Algorithm::RS256), &claims, &key).context("Failed to sign JWT")?;
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
) -> anyhow::Result<String> {
    create_installation_access_token_with_permissions_and_install_url(
        client,
        jwt,
        owner,
        repo,
        base_url,
        permissions,
        None,
    )
    .await
}

pub async fn create_installation_access_token_with_permissions_and_install_url(
    client: &impl HttpClient,
    jwt: &str,
    owner: &str,
    repo: &str,
    base_url: &str,
    permissions: serde_json::Value,
    install_url: Option<&str>,
) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct Installation {
        id: u64,
    }

    #[derive(Deserialize)]
    struct AccessToken {
        token: String,
    }

    // Step 1: Find the installation for this repo
    let installation_endpoint = format!("{base_url}/repos/{owner}/{repo}/installation");
    let auth = format!("Bearer {jwt}");
    let resp = client
        .request(
            HttpMethod::Get,
            &installation_endpoint,
            &github_headers(&auth),
            None,
        )
        .await
        .context("Failed to look up GitHub App installation")?;

    match resp.status {
        200 => {}
        404 => {
            let install_url = install_url.map_or_else(
                || format!("https://github.com/organizations/{owner}/settings/installations"),
                str::to_string,
            );
            bail!(
                "GitHub App is not installed for {owner}. \
                 Install it at {install_url}"
            );
        }
        403 => {
            bail!(
                "GitHub App installation is suspended. \
                 Re-enable it in your organization's GitHub App settings."
            );
        }
        401 => {
            bail!(
                "GitHub App authentication failed. \
                 Check that app_id and GITHUB_APP_PRIVATE_KEY are correct."
            );
        }
        _ => {
            bail!(
                "Unexpected status {} looking up GitHub App installation",
                resp.status
            );
        }
    }

    let installation: Installation = resp
        .json()
        .context("Failed to parse installation response")?;

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
        .context("Failed to create installation access token")?;

    match token_resp.status {
        201 => {}
        422 => {
            bail!(
                "GitHub App does not have access to repository {repo}. \
                 Update the installation's repository permissions to include it."
            );
        }
        401 => {
            bail!(
                "GitHub App authentication failed. \
                 Check that app_id and GITHUB_APP_PRIVATE_KEY are correct."
            );
        }
        _ => {
            bail!(
                "Unexpected status {} creating installation access token",
                token_resp.status
            );
        }
    }

    let access_token: AccessToken = token_resp
        .json()
        .context("Failed to parse access token response")?;

    Ok(access_token.token)
}

/// Request a scoped Installation Access Token with `contents: write`.
pub async fn create_installation_access_token(
    client: &impl HttpClient,
    jwt: &str,
    owner: &str,
    repo: &str,
    base_url: &str,
) -> anyhow::Result<String> {
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
) -> anyhow::Result<String> {
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
    pub number:   u64,
    pub node_id:  String,
}

/// Create a pull request on GitHub.
///
/// Signs a JWT, obtains a PR-scoped installation token, and POSTs to the
/// GitHub pulls API.
#[allow(
    clippy::too_many_arguments,
    reason = "Creating a pull request needs explicit repo, branch, and body fields."
)]
pub async fn create_pull_request(
    ctx: &GitHubContext<'_>,
    owner: &str,
    repo: &str,
    base: &str,
    head: &str,
    title: &str,
    body: &str,
    draft: bool,
) -> anyhow::Result<CreatedPullRequest> {
    let client = ctx.http_client()?;
    create_pull_request_with_client(&client, ctx, owner, repo, base, head, title, body, draft).await
}

#[allow(
    clippy::too_many_arguments,
    reason = "Creating a pull request needs explicit repo, branch, and body fields."
)]
pub async fn create_pull_request_with_client(
    client: &impl HttpClient,
    ctx: &GitHubContext<'_>,
    owner: &str,
    repo: &str,
    base: &str,
    head: &str,
    title: &str,
    body: &str,
    draft: bool,
) -> anyhow::Result<CreatedPullRequest> {
    #[derive(Deserialize)]
    struct PullRequestResponse {
        html_url: String,
        number:   u64,
        node_id:  String,
    }

    let token = ctx
        .creds
        .resolve_bearer_token(
            client,
            owner,
            repo,
            ctx.base_url,
            serde_json::json!({ "contents": "write", "pull_requests": "write" }),
        )
        .await?;

    tracing::info!(title = %title, head = %head, base = %base, draft, "Creating pull request");

    let pr_body = serde_json::json!({
        "title": title,
        "head": head,
        "base": base,
        "body": body,
        "draft": draft,
    });

    let url = format!("{}/repos/{owner}/{repo}/pulls", ctx.base_url);
    let auth = format!("Bearer {token}");
    let resp = HttpClient::request(
        client,
        HttpMethod::Post,
        &url,
        &github_headers(&auth),
        Some(&pr_body),
    )
    .await
    .context("Failed to create pull request")?;

    match resp.status {
        201 => {}
        422 => {
            bail!("Pull request could not be created (422): {}", resp.text());
        }
        401 | 403 => {
            bail!(
                "Authentication failed creating pull request ({})",
                resp.status
            );
        }
        _ => {
            bail!(
                "Unexpected status {} creating pull request: {}",
                resp.status,
                resp.text()
            );
        }
    }

    let pr: PullRequestResponse = resp
        .json()
        .context("Failed to parse pull request response")?;

    Ok(CreatedPullRequest {
        html_url: pr.html_url,
        number:   pr.number,
        node_id:  pr.node_id,
    })
}

fn merge_method_as_graphql_value(method: MergeStrategy) -> &'static str {
    match method {
        MergeStrategy::Merge => "MERGE",
        MergeStrategy::Squash => "SQUASH",
        MergeStrategy::Rebase => "REBASE",
    }
}

/// Enable auto-merge on a pull request via GitHub's GraphQL API.
///
/// Requires the PR's `node_id` (from the REST API response) and a merge method.
/// The repository must have auto-merge enabled in its settings.
pub async fn enable_auto_merge(
    ctx: &GitHubContext<'_>,
    owner: &str,
    repo: &str,
    pr_node_id: &str,
    merge_method: MergeStrategy,
) -> anyhow::Result<()> {
    let client = ctx.http_client()?;
    enable_auto_merge_with_client(&client, ctx, owner, repo, pr_node_id, merge_method).await
}

pub async fn enable_auto_merge_with_client(
    client: &impl HttpClient,
    ctx: &GitHubContext<'_>,
    owner: &str,
    repo: &str,
    pr_node_id: &str,
    merge_method: MergeStrategy,
) -> anyhow::Result<()> {
    let token = ctx
        .creds
        .resolve_bearer_token(
            client,
            owner,
            repo,
            ctx.base_url,
            serde_json::json!({ "contents": "write", "pull_requests": "write" }),
        )
        .await?;

    let graphql_value = merge_method_as_graphql_value(merge_method);
    let query = format!(
        r#"mutation {{
  enablePullRequestAutoMerge(input: {{pullRequestId: "{pr_node_id}", mergeMethod: {graphql_value}}}) {{
    pullRequest {{
      autoMergeRequest {{
        enabledAt
        mergeMethod
      }}
    }}
  }}
}}"#,
    );

    tracing::debug!(
        pr_node_id,
        merge_method = graphql_value,
        "Enabling auto-merge"
    );

    let graphql_url = format!("{}/graphql", ctx.base_url);
    let auth = format!("Bearer {token}");
    let graphql_body = serde_json::json!({ "query": query });
    let resp = HttpClient::request(
        client,
        HttpMethod::Post,
        &graphql_url,
        &[("Authorization", auth.as_str()), ("User-Agent", "fabro")],
        Some(&graphql_body),
    )
    .await
    .context("Failed to enable auto-merge")?;

    let status = resp.status;
    let body: serde_json::Value = resp.json().context("Failed to parse auto-merge response")?;

    if !(200..300).contains(&status) {
        bail!("Auto-merge request failed ({status}): {body}");
    }

    if let Some(errors) = body.get("errors") {
        bail!("Auto-merge GraphQL error: {errors}");
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

pub fn normalize_repo_origin_url(url: &str) -> String {
    let https = ssh_url_to_https(url.trim());
    let without_credentials = strip_https_credentials(&https);
    let normalized = normalize_https_host_path(&without_credentials);
    let normalized = normalized.trim_end_matches('/');
    normalized
        .strip_suffix(".git")
        .unwrap_or(normalized)
        .to_string()
}

fn strip_https_credentials(url: &str) -> String {
    let Some(rest) = url.strip_prefix("https://") else {
        return url.to_string();
    };

    match rest.split_once('@') {
        Some((before, after)) if !before.contains('/') => format!("https://{after}"),
        _ => url.to_string(),
    }
}

fn normalize_https_host_path(url: &str) -> String {
    let Some(rest) = url.strip_prefix("https://") else {
        return url.to_string();
    };

    match rest.split_once(':') {
        Some((host, path)) if !host.contains('/') && !path.starts_with('/') => {
            format!("https://{host}/{path}")
        }
        _ => url.to_string(),
    }
}

/// Check whether a branch exists in a GitHub repository.
///
/// Uses a GitHub App installation token to query the branches API.
/// Returns `true` if the branch exists, `false` if it doesn't (404).
pub async fn branch_exists(
    ctx: &GitHubContext<'_>,
    owner: &str,
    repo: &str,
    branch: &str,
) -> anyhow::Result<bool> {
    let client = ctx.http_client()?;
    branch_exists_with_client(&client, ctx, owner, repo, branch).await
}

async fn branch_exists_with_client(
    client: &impl HttpClient,
    ctx: &GitHubContext<'_>,
    owner: &str,
    repo: &str,
    branch: &str,
) -> anyhow::Result<bool> {
    let token = ctx
        .creds
        .resolve_bearer_token(
            client,
            owner,
            repo,
            ctx.base_url,
            serde_json::json!({ "contents": "write" }),
        )
        .await?;

    let url = format!("{}/repos/{owner}/{repo}/branches/{branch}", ctx.base_url);
    let auth = format!("Bearer {token}");
    let resp = client
        .request(HttpMethod::Get, &url, &github_headers(&auth), None)
        .await
        .context("Failed to check branch existence")?;

    match resp.status {
        200 => Ok(true),
        404 => Ok(false),
        status => bail!("Unexpected status {status} checking branch '{branch}'"),
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
) -> anyhow::Result<bool> {
    let url = format!("{base_url}/repos/{owner}/{repo}/installation");
    let auth = format!("Bearer {jwt}");
    let resp = client
        .request(HttpMethod::Get, &url, &github_headers(&auth), None)
        .await
        .context("Failed to check GitHub App installation")?;

    match resp.status {
        200 => Ok(true),
        404 => Ok(false),
        401 => bail!(
            "GitHub App authentication failed. \
             Check that app_id and GITHUB_APP_PRIVATE_KEY are correct."
        ),
        403 => bail!(
            "GitHub App installation is suspended. \
             Re-enable it in your organization's GitHub App settings."
        ),
        status => bail!("Unexpected status {status} checking GitHub App installation"),
    }
}

/// Fetch information about the authenticated GitHub App.
///
/// Uses the App JWT to call `GET /app` and returns the app's slug and owner.
pub async fn get_authenticated_app(
    client: &impl HttpClient,
    jwt: &str,
    base_url: &str,
) -> anyhow::Result<AppInfo> {
    let url = format!("{base_url}/app");
    let auth = format!("Bearer {jwt}");
    let resp = client
        .request(HttpMethod::Get, &url, &github_headers(&auth), None)
        .await
        .context("Failed to fetch GitHub App info")?;

    match resp.status {
        200 => {}
        401 => {
            bail!(
                "GitHub App authentication failed. \
                 Check that app_id and GITHUB_APP_PRIVATE_KEY are correct."
            );
        }
        status => {
            bail!("Unexpected status {status} fetching GitHub App info");
        }
    }

    resp.json::<AppInfo>()
        .context("Failed to parse GitHub App info")
}

/// Update a GitHub App's webhook URL via `PATCH /app/hook/config`.
///
/// Signs an App JWT and sets the webhook endpoint and content type.
pub async fn update_app_webhook_config(
    app_id: &str,
    private_key_pem: &str,
    webhook_url: &str,
) -> anyhow::Result<()> {
    let jwt = sign_app_jwt(app_id, private_key_pem)?;
    let client = http_client()?;
    let url = format!("{}/app/hook/config", github_api_base_url());
    let auth = format!("Bearer {jwt}");
    let body = serde_json::json!({
        "url": webhook_url,
        "content_type": "json",
    });

    let resp = HttpClient::request(
        &client,
        HttpMethod::Patch,
        &url,
        &github_headers(&auth),
        Some(&body),
    )
    .await
    .context("Failed to update GitHub App webhook")?;

    if !(200..300).contains(&resp.status) {
        bail!("GitHub API returned {}: {}", resp.status, resp.text());
    }

    Ok(())
}

/// Resolve git clone credentials for a GitHub repository.
///
/// Returns `(username, password)` for authenticated cloning.
/// Always generates a token regardless of repo visibility, since the token
/// is needed for pushing from the sandbox.
pub async fn resolve_clone_credentials(
    ctx: &GitHubContext<'_>,
    owner: &str,
    repo: &str,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    let token = match ctx.creds {
        GitHubCredentials::Token(token) => token.clone(),
        GitHubCredentials::App(_) => {
            let client = ctx.http_client()?;
            ctx.creds
                .resolve_bearer_token(
                    &client,
                    owner,
                    repo,
                    ctx.base_url,
                    serde_json::json!({ "contents": "write" }),
                )
                .await?
        }
    };
    Ok((Some("x-access-token".to_string()), Some(token)))
}

/// Embed a token into an HTTPS URL for authenticated git operations.
///
/// Converts `https://github.com/owner/repo` to
/// `https://x-access-token:<token>@github.com/owner/repo`.
pub fn embed_token_in_url(url: &str, token: &str) -> anyhow::Result<DisplaySafeUrl> {
    let mut url = DisplaySafeUrl::parse(url).context("Failed to parse GitHub HTTPS URL")?;
    if url.scheme() != "https" {
        bail!("GitHub clone URL must use HTTPS: {}", url.redacted_string());
    }
    url.set_username("x-access-token")
        .map_err(|()| anyhow!("Failed to set GitHub token username"))?;
    url.set_password(Some(token))
        .map_err(|()| anyhow!("Failed to set GitHub token password"))?;
    Ok(url)
}

/// Resolve an authenticated HTTPS URL for a GitHub repository.
///
/// Parses owner/repo from the URL, obtains a fresh installation access token,
/// and returns the URL with embedded credentials.
pub async fn resolve_authenticated_url(
    ctx: &GitHubContext<'_>,
    url: &str,
) -> anyhow::Result<DisplaySafeUrl> {
    let (owner, repo) = parse_github_owner_repo(url)?;
    let (_username, password) = resolve_clone_credentials(ctx, &owner, &repo).await?;
    match password {
        Some(token) => embed_token_in_url(url, &token),
        None => DisplaySafeUrl::parse(url).context("Failed to parse GitHub HTTPS URL"),
    }
}

/// Fetch detailed information about a pull request.
pub async fn get_pull_request(
    ctx: &GitHubContext<'_>,
    owner: &str,
    repo: &str,
    number: u64,
) -> Result<PullRequestGithubDetail, PullRequestApiError> {
    let client = ctx.http_client()?;
    get_pull_request_with_client(&client, ctx, owner, repo, number).await
}

pub async fn get_pull_request_with_client(
    client: &impl HttpClient,
    ctx: &GitHubContext<'_>,
    owner: &str,
    repo: &str,
    number: u64,
) -> Result<PullRequestGithubDetail, PullRequestApiError> {
    tracing::debug!(owner, repo, number, "Fetching pull request");

    let token = ctx
        .creds
        .resolve_bearer_token(
            client,
            owner,
            repo,
            ctx.base_url,
            serde_json::json!({ "contents": "write", "pull_requests": "write" }),
        )
        .await?;

    let url = format!("{}/repos/{owner}/{repo}/pulls/{number}", ctx.base_url);
    let auth = format!("Bearer {token}");
    let resp = client
        .request(HttpMethod::Get, &url, &github_headers(&auth), None)
        .await
        .context("Failed to fetch pull request")?;

    match resp.status {
        200 => {}
        404 => {
            return Err(PullRequestApiError::NotFound {
                owner: owner.to_string(),
                repo: repo.to_string(),
                number,
            });
        }
        401 | 403 => {
            return Err(anyhow!(
                "Authentication failed fetching pull request ({})",
                resp.status
            )
            .into());
        }
        status => {
            return Err(anyhow!(
                "Unexpected status {status} fetching pull request: {}",
                resp.text()
            )
            .into());
        }
    }

    Ok(resp
        .json::<PullRequestGithubDetail>()
        .context("Failed to parse pull request response")?)
}

/// Merge a pull request.
pub async fn merge_pull_request(
    ctx: &GitHubContext<'_>,
    owner: &str,
    repo: &str,
    number: u64,
    method: MergeStrategy,
) -> Result<(), PullRequestApiError> {
    let client = ctx.http_client()?;
    merge_pull_request_with_client(&client, ctx, owner, repo, number, method).await
}

pub async fn merge_pull_request_with_client(
    client: &impl HttpClient,
    ctx: &GitHubContext<'_>,
    owner: &str,
    repo: &str,
    number: u64,
    method: MergeStrategy,
) -> Result<(), PullRequestApiError> {
    tracing::debug!(owner, repo, number, method = %method, "Merging pull request");

    let token = ctx
        .creds
        .resolve_bearer_token(
            client,
            owner,
            repo,
            ctx.base_url,
            serde_json::json!({ "contents": "write", "pull_requests": "write" }),
        )
        .await?;

    let url = format!("{}/repos/{owner}/{repo}/pulls/{number}/merge", ctx.base_url);
    let body = serde_json::json!({ "merge_method": method });
    let auth = format!("Bearer {token}");

    let resp = client
        .request(HttpMethod::Put, &url, &github_headers(&auth), Some(&body))
        .await
        .context("Failed to merge pull request")?;

    match resp.status {
        200 => Ok(()),
        405 => Err(
            anyhow!("Pull request #{number} is not mergeable (method may not be allowed)").into(),
        ),
        409 => Err(anyhow!("Pull request #{number} has a merge conflict").into()),
        404 => Err(PullRequestApiError::NotFound {
            owner: owner.to_string(),
            repo: repo.to_string(),
            number,
        }),
        401 | 403 => Err(anyhow!(
            "Authentication failed merging pull request ({})",
            resp.status
        )
        .into()),
        status => Err(anyhow!(
            "Unexpected status {status} merging pull request: {}",
            resp.text()
        )
        .into()),
    }
}

/// Close a pull request.
pub async fn close_pull_request(
    ctx: &GitHubContext<'_>,
    owner: &str,
    repo: &str,
    number: u64,
) -> Result<(), PullRequestApiError> {
    let client = ctx.http_client()?;
    close_pull_request_with_client(&client, ctx, owner, repo, number).await
}

pub async fn close_pull_request_with_client(
    client: &impl HttpClient,
    ctx: &GitHubContext<'_>,
    owner: &str,
    repo: &str,
    number: u64,
) -> Result<(), PullRequestApiError> {
    tracing::debug!(owner, repo, number, "Closing pull request");

    let token = ctx
        .creds
        .resolve_bearer_token(
            client,
            owner,
            repo,
            ctx.base_url,
            serde_json::json!({ "contents": "write", "pull_requests": "write" }),
        )
        .await?;

    let url = format!("{}/repos/{owner}/{repo}/pulls/{number}", ctx.base_url);
    let body = serde_json::json!({ "state": "closed" });
    let auth = format!("Bearer {token}");

    let resp = client
        .request(HttpMethod::Patch, &url, &github_headers(&auth), Some(&body))
        .await
        .context("Failed to close pull request")?;

    match resp.status {
        200 => Ok(()),
        404 => Err(PullRequestApiError::NotFound {
            owner: owner.to_string(),
            repo: repo.to_string(),
            number,
        }),
        401 | 403 => Err(anyhow!(
            "Authentication failed closing pull request ({})",
            resp.status
        )
        .into()),
        status => Err(anyhow!(
            "Unexpected status {status} closing pull request: {}",
            resp.text()
        )
        .into()),
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
) -> anyhow::Result<String> {
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
    use std::io;
    use std::sync::{Arc, Mutex};

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use tracing::{debug, subscriber};
    use tracing_subscriber::fmt::{self as tracing_fmt, MakeWriter};
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::registry;

    use super::*;

    #[test]
    fn decode_pem_env_accepts_raw_pem() {
        let pem = "-----BEGIN TEST KEY-----\nabc\n-----END TEST KEY-----";
        assert_eq!(decode_pem_env("GITHUB_APP_PRIVATE_KEY", pem).unwrap(), pem);
    }

    #[test]
    fn pull_request_api_error_preserves_other_source_chain() {
        let original = anyhow::Error::new(std::io::Error::other("leaf failure"))
            .context("middle context")
            .context("outer context");
        let original_chain = original
            .chain()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        let err = PullRequestApiError::from(original);
        let wrapped = anyhow::Error::new(err);
        let wrapped_chain = wrapped.chain().map(ToString::to_string).collect::<Vec<_>>();

        assert_eq!(wrapped_chain.len(), original_chain.len());
        for original_cause in original_chain {
            assert!(
                wrapped_chain.iter().any(|cause| cause == &original_cause),
                "missing original cause {original_cause:?} in {wrapped_chain:#?}"
            );
        }
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
    fn normalize_repo_origin_url_converts_ssh_and_trims_git_suffix() {
        assert_eq!(
            normalize_repo_origin_url("git@github.com:brynary/arc.git"),
            "https://github.com/brynary/arc"
        );
    }

    #[test]
    fn normalize_repo_origin_url_strips_credentials_and_trailing_slash() {
        assert_eq!(
            normalize_repo_origin_url("https://token@github.com/acme/widgets.git/"),
            "https://github.com/acme/widgets"
        );
    }

    #[test]
    fn normalize_repo_origin_url_handles_sanitized_git_at_shape() {
        assert_eq!(
            normalize_repo_origin_url("https://***@github.com:acme/widgets.git"),
            "https://github.com/acme/widgets"
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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Not a GitHub HTTPS URL")
        );
    }

    #[test]
    fn embed_token_in_url_redacts_display_and_keeps_raw_access() {
        let url = embed_token_in_url("https://github.com/acme/widgets.git", "ghs_abc123").unwrap();

        assert_eq!(
            url.redacted_string(),
            "https://x-access-token:****@github.com/acme/widgets.git"
        );
        assert_eq!(
            url.raw_string(),
            "https://x-access-token:ghs_abc123@github.com/acme/widgets.git"
        );
    }

    #[test]
    fn logging_embedded_token_url_does_not_emit_token() {
        let output = CapturedTrace::default();
        let subscriber = registry().with(
            tracing_fmt::layer()
                .with_writer(output.clone())
                .without_time()
                .with_target(false),
        );

        subscriber::with_default(subscriber, || {
            let url =
                embed_token_in_url("https://github.com/acme/widgets.git", "ghs_abc123").unwrap();
            debug!(?url, %url, "resolved authenticated GitHub URL");
        });

        let formatted = output.captured_output();
        assert!(formatted.contains("x-access-token:****@"));
        assert!(!formatted.contains("ghs_abc123"));
    }

    #[derive(Clone, Default)]
    struct CapturedTrace {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    impl CapturedTrace {
        fn captured_output(&self) -> String {
            let buffer = self.buffer.lock().unwrap();
            String::from_utf8(buffer.clone()).unwrap()
        }
    }

    impl<'writer> MakeWriter<'writer> for CapturedTrace {
        type Writer = CapturedTraceWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            CapturedTraceWriter {
                buffer: Arc::clone(&self.buffer),
            }
        }
    }

    struct CapturedTraceWriter {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    impl io::Write for CapturedTraceWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.buffer.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn parse_non_github_url_errors() {
        let result = parse_github_owner_repo("https://gitlab.com/owner/repo");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Not a GitHub HTTPS URL")
        );
    }

    #[test]
    fn parse_missing_repo_errors() {
        let result = parse_github_owner_repo("https://github.com/owner");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Missing repo"));
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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid RSA private key")
        );
    }

    // -----------------------------------------------------------------------
    // MockHttpClient
    // -----------------------------------------------------------------------

    struct MockRoute {
        method:           HttpMethod,
        path:             String,
        status:           u16,
        response_body:    String,
        assert_header:    Option<(String, MockHeaderCheck)>,
        assert_body_json: Option<serde_json::Value>,
    }

    enum MockHeaderCheck {
        Equals(String),
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
        ) -> anyhow::Result<HttpResponse> {
            for route in &self.routes {
                if method == route.method && url.ends_with(&route.path) {
                    if let Some((name, MockHeaderCheck::Equals(expected))) = &route.assert_header {
                        let (_, v) = headers
                            .iter()
                            .find(|(k, _)| *k == name.as_str())
                            .unwrap_or_else(|| {
                                panic!("Expected header '{name}' not found in request to {url}")
                            });
                        assert_eq!(*v, expected.as_str(), "Header '{name}' mismatch for {url}");
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
            .unwrap_err()
            .to_string();
        assert!(err.contains("not installed"), "got: {err}");
        assert!(err.contains("owner"), "got: {err}");
    }

    #[tokio::test]
    async fn create_iat_not_installed_uses_app_specific_install_url() {
        let mock =
            MockHttpClient::new().on(HttpMethod::Get, "/repos/owner/repo/installation", 404, "");
        let install_url =
            "https://github.com/organizations/owner/settings/apps/fabro-test/installations";

        let err = create_installation_access_token_with_permissions_and_install_url(
            &mock,
            "jwt",
            "owner",
            "repo",
            "",
            serde_json::json!({ "contents": "write" }),
            Some(install_url),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("not installed"), "got: {err}");
        assert!(err.contains(install_url), "got: {err}");
        assert!(
            !err.contains("https://github.com/organizations/owner/settings/installations"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn create_iat_suspended() {
        let mock =
            MockHttpClient::new().on(HttpMethod::Get, "/repos/owner/repo/installation", 403, "");

        let err = create_installation_access_token(&mock, "jwt", "owner", "repo", "")
            .await
            .unwrap_err()
            .to_string();
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
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not have access"), "got: {err}");
        assert!(err.contains("repo"), "got: {err}");
    }

    #[tokio::test]
    async fn create_iat_auth_failed() {
        let mock =
            MockHttpClient::new().on(HttpMethod::Get, "/repos/owner/repo/installation", 401, "");

        let err = create_installation_access_token(&mock, "jwt", "owner", "repo", "")
            .await
            .unwrap_err()
            .to_string();
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
        let creds = GitHubCredentials::App(GitHubAppCredentials {
            app_id:          "test".to_string(),
            private_key_pem: pem.to_string(),
            slug:            None,
        });
        let result = branch_exists_with_client(
            &mock,
            &GitHubContext::new(&creds, ""),
            "owner",
            "repo",
            "my-branch",
        )
        .await;
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
        let creds = GitHubCredentials::App(GitHubAppCredentials {
            app_id:          "test".to_string(),
            private_key_pem: pem.to_string(),
            slug:            None,
        });
        let result = branch_exists_with_client(
            &mock,
            &GitHubContext::new(&creds, ""),
            "owner",
            "repo",
            "no-such-branch",
        )
        .await;
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
        let creds = GitHubCredentials::App(GitHubAppCredentials {
            app_id:          "test".to_string(),
            private_key_pem: pem.to_string(),
            slug:            None,
        });
        let result = branch_exists_with_client(
            &mock,
            &GitHubContext::new(&creds, ""),
            "owner",
            "repo",
            "broken",
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn branch_exists_with_token_uses_direct_bearer_token() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/branches/my-branch",
                200,
                r#"{"name": "my-branch"}"#,
            )
            .with_req_header("Authorization", "Bearer ghu_test");

        let creds = GitHubCredentials::Token("ghu_test".to_string());
        let result = branch_exists_with_client(
            &mock,
            &GitHubContext::new(&creds, ""),
            "owner",
            "repo",
            "my-branch",
        )
        .await;

        assert!(result.unwrap());
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
            result
                .unwrap_err()
                .to_string()
                .contains("authentication failed"),
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
            result
                .unwrap_err()
                .to_string()
                .contains("authentication failed"),
            "expected auth error"
        );
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
            "merged": false,
            "merged_at": null,
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
        let creds = GitHubCredentials::App(GitHubAppCredentials {
            app_id:          "test".to_string(),
            private_key_pem: pem.to_string(),
            slug:            None,
        });
        let detail = get_pull_request_with_client(
            &mock,
            &GitHubContext::new(&creds, ""),
            "owner",
            "repo",
            42,
        )
        .await
        .unwrap();

        assert_eq!(detail.number, 42);
        assert_eq!(detail.title, "Fix the bug");
        assert_eq!(detail.state, "open");
        assert!(!detail.merged);
        assert_eq!(detail.merged_at, None);
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
        let creds = GitHubCredentials::App(GitHubAppCredentials {
            app_id:          "test".to_string(),
            private_key_pem: pem.to_string(),
            slug:            None,
        });
        let err = get_pull_request_with_client(
            &mock,
            &GitHubContext::new(&creds, ""),
            "owner",
            "repo",
            999,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err,
                PullRequestApiError::NotFound {
                    number: 999,
                    ref owner,
                    ref repo,
                } if owner == "owner" && repo == "repo"
            ),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn get_pr_with_token_uses_direct_bearer_token() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/repos/owner/repo/pulls/42",
                200,
                mock_pr_json(),
            )
            .with_req_header("Authorization", "Bearer ghu_test");

        let creds = GitHubCredentials::Token("ghu_test".to_string());
        let detail = get_pull_request_with_client(
            &mock,
            &GitHubContext::new(&creds, ""),
            "owner",
            "repo",
            42,
        )
        .await
        .unwrap();

        assert_eq!(detail.number, 42);
    }

    #[tokio::test]
    async fn resolve_clone_credentials_returns_token_for_token_credentials() {
        let creds = GitHubCredentials::Token("ghu_test".to_string());

        let credentials =
            resolve_clone_credentials(&GitHubContext::new(&creds, ""), "owner", "repo")
                .await
                .unwrap();

        assert_eq!(
            credentials,
            (
                Some("x-access-token".to_string()),
                Some("ghu_test".to_string())
            )
        );
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
        let creds = GitHubCredentials::App(GitHubAppCredentials {
            app_id:          "test".to_string(),
            private_key_pem: pem.to_string(),
            slug:            None,
        });
        merge_pull_request_with_client(
            &mock,
            &GitHubContext::new(&creds, ""),
            "owner",
            "repo",
            42,
            MergeStrategy::Squash,
        )
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
        let creds = GitHubCredentials::App(GitHubAppCredentials {
            app_id:          "test".to_string(),
            private_key_pem: pem.to_string(),
            slug:            None,
        });
        let err = merge_pull_request_with_client(
            &mock,
            &GitHubContext::new(&creds, ""),
            "owner",
            "repo",
            42,
            MergeStrategy::Squash,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("not mergeable"), "got: {err}");
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
        let creds = GitHubCredentials::App(GitHubAppCredentials {
            app_id:          "test".to_string(),
            private_key_pem: pem.to_string(),
            slug:            None,
        });
        let err = merge_pull_request_with_client(
            &mock,
            &GitHubContext::new(&creds, ""),
            "owner",
            "repo",
            42,
            MergeStrategy::Squash,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("merge conflict"), "got: {err}");
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
        let creds = GitHubCredentials::App(GitHubAppCredentials {
            app_id:          "test".to_string(),
            private_key_pem: pem.to_string(),
            slug:            None,
        });
        close_pull_request_with_client(&mock, &GitHubContext::new(&creds, ""), "owner", "repo", 42)
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
        let creds = GitHubCredentials::App(GitHubAppCredentials {
            app_id:          "test".to_string(),
            private_key_pem: pem.to_string(),
            slug:            None,
        });
        let err = close_pull_request_with_client(
            &mock,
            &GitHubContext::new(&creds, ""),
            "owner",
            "repo",
            999,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err,
                PullRequestApiError::NotFound {
                    number: 999,
                    ref owner,
                    ref repo,
                } if owner == "owner" && repo == "repo"
            ),
            "got: {err}"
        );
    }
}
