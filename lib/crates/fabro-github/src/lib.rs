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
                "Authentication failed creating pull request ({status})"
            ));
        }
        _ => {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(format!(
                "Unexpected status {status} creating pull request: {body_text}"
            ));
        }
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

/// Fetch information about the authenticated GitHub App.
///
/// Uses the App JWT to call `GET /app` and returns the app's slug and owner.
pub async fn get_authenticated_app(
    client: &reqwest::Client,
    jwt: &str,
    base_url: &str,
) -> Result<AppInfo, String> {
    let url = format!("{base_url}/app");
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch GitHub App info: {e}"))?;

    match resp.status().as_u16() {
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
        .await
        .map_err(|e| format!("Failed to parse GitHub App info: {e}"))
}

/// Check whether a GitHub App is publicly visible.
///
/// Calls `GET /apps/{slug}` **without** authentication. Public apps return 200,
/// private apps return 404 to unauthenticated requests.
pub async fn is_app_public(
    client: &reqwest::Client,
    slug: &str,
    base_url: &str,
) -> Result<bool, String> {
    let url = format!("{base_url}/apps/{slug}");
    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "fabro")
        .send()
        .await
        .map_err(|e| format!("Failed to check GitHub App visibility: {e}"))?;

    match resp.status().as_u16() {
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
            ));
        }
        401 | 403 => {
            return Err(format!(
                "Authentication failed fetching pull request ({})",
                resp.status()
            ));
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
    // get_authenticated_app
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_authenticated_app_success() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/app")
            .match_header("Authorization", "Bearer test-jwt")
            .with_status(200)
            .with_body(r#"{"slug": "my-fabro-app", "owner": {"login": "my-org"}}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let info = get_authenticated_app(&client, "test-jwt", &server.url())
            .await
            .unwrap();
        assert_eq!(info.slug, "my-fabro-app");
        assert_eq!(info.owner.login, "my-org");
    }

    #[tokio::test]
    async fn get_authenticated_app_auth_failure() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/app")
            .with_status(401)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let result = get_authenticated_app(&client, "bad-jwt", &server.url()).await;
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
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/apps/my-fabro-app")
            .with_status(200)
            .with_body(r#"{"slug": "my-fabro-app"}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let result = is_app_public(&client, "my-fabro-app", &server.url()).await;
        assert_eq!(result.unwrap(), true);
    }

    #[tokio::test]
    async fn is_app_public_returns_false_on_404() {
        let mut server = mockito::Server::new_async().await;

        server
            .mock("GET", "/apps/my-private-app")
            .with_status(404)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let result = is_app_public(&client, "my-private-app", &server.url()).await;
        assert_eq!(result.unwrap(), false);
    }

    #[tokio::test]
    async fn is_app_public_no_auth_header() {
        let mut server = mockito::Server::new_async().await;

        // Verify the request does NOT include an Authorization header
        let mock = server
            .mock("GET", "/apps/my-app")
            .match_header("Authorization", mockito::Matcher::Missing)
            .with_status(200)
            .with_body(r#"{"slug": "my-app"}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let result = is_app_public(&client, "my-app", &server.url()).await;
        assert_eq!(result.unwrap(), true);

        mock.assert_async().await;
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
}
