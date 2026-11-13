//! GitHub account identity lookups for a run's Git author.
//!
//! A run's commits carry the identity of the credential the run uses:
//! the App's bot account (`<slug>[bot]`) or the authenticated user of a
//! personal access token. Both resolve to a stable noreply
//! address, `{id}+{login}@users.noreply.<host>`, so GitHub attributes
//! the commits to that account without a private email.

use anyhow::{Context as _, bail};
use fabro_types::GitHubHost;
use serde::Deserialize;

use crate::{GitHubAppCredentials, HttpClient, HttpMethod, github_headers, sign_app_jwt};

/// A GitHub account resolved for Git attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubAccountIdentity {
    /// The account login. App bot accounts carry the `[bot]` suffix.
    pub login: String,
    /// The numeric user id (a bot's *user* id, not the App id).
    pub id:    u64,
}

impl GitHubAccountIdentity {
    /// The noreply address `host` attributes to this account.
    #[must_use]
    pub fn noreply_email(&self, host: &GitHubHost) -> String {
        format!("{}+{}@{}", self.id, self.login, host.noreply_email_domain())
    }
}

#[derive(Deserialize)]
struct AccountResponse {
    login: String,
    id:    u64,
}

fn validated(account: &AccountResponse, what: &str) -> anyhow::Result<GitHubAccountIdentity> {
    let login = account.login.trim();
    if login.is_empty() {
        bail!("GitHub returned an empty login for {what}");
    }
    if login
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace() || matches!(ch, '<' | '>' | '@'))
    {
        bail!("GitHub returned an invalid login for {what}");
    }
    if account.id == 0 {
        bail!("GitHub returned an invalid user id for {what}");
    }
    Ok(GitHubAccountIdentity {
        login: login.to_string(),
        id:    account.id,
    })
}

/// Resolve the authenticated user of a personal access token via `GET /user`.
///
/// Reads only the public login and id; no email scope is requested.
pub async fn lookup_token_identity(
    client: &impl HttpClient,
    token: &str,
    base_url: &str,
) -> anyhow::Result<GitHubAccountIdentity> {
    let url = format!("{base_url}/user");
    let auth = format!("Bearer {token}");
    let resp = client
        .request(HttpMethod::Get, &url, &github_headers(&auth), None)
        .await
        .context("Failed to fetch the GitHub token's user")?;
    match resp.status {
        200 => {}
        401 => bail!("GitHub rejected the configured GITHUB_TOKEN while resolving its user"),
        403 => bail!("GitHub refused to identify the configured GITHUB_TOKEN's user (403)"),
        status => bail!("Unexpected status {status} fetching the GitHub token's user"),
    }
    let account: AccountResponse = resp
        .json()
        .context("Failed to parse the GitHub token's user")?;
    validated(&account, "the GitHub token's user")
}

/// Resolve the bot account of a GitHub App.
///
/// The App slug comes from the credentials when configured, otherwise from
/// `GET /app` with the App JWT. The bot account is then read from
/// `GET /users/{slug}[bot]`. That endpoint rejects App JWTs, so it is called
/// with `bearer` (an installation token or other API token) when one is
/// available and anonymously otherwise.
pub async fn lookup_app_bot_identity(
    client: &impl HttpClient,
    creds: &GitHubAppCredentials,
    base_url: &str,
    bearer: Option<&str>,
) -> anyhow::Result<GitHubAccountIdentity> {
    let slug = match creds.slug.as_deref().map(str::trim) {
        Some(slug) if !slug.is_empty() => slug.to_string(),
        _ => {
            let jwt = sign_app_jwt(&creds.app_id, &creds.private_key_pem)?;
            crate::get_authenticated_app(client, &jwt, base_url)
                .await
                .context("Failed to determine the GitHub App slug")?
                .slug
        }
    };
    let login = format!("{slug}[bot]");
    let url = format!(
        "{base_url}/users/{}",
        login.replace('[', "%5B").replace(']', "%5D")
    );
    let auth = bearer.map(|token| format!("Bearer {token}"));
    let headers: Vec<(&str, &str)> = match auth.as_deref() {
        Some(auth) => github_headers(auth).to_vec(),
        None => vec![
            ("Accept", "application/vnd.github+json"),
            ("User-Agent", "fabro"),
        ],
    };
    let resp = client
        .request(HttpMethod::Get, &url, &headers, None)
        .await
        .with_context(|| format!("Failed to fetch the GitHub App bot account {login}"))?;
    match resp.status {
        200 => {}
        404 => bail!(
            "GitHub has no bot account for App slug {slug}; check server.integrations.github.slug"
        ),
        status => bail!("Unexpected status {status} fetching the GitHub App bot account {login}"),
    }
    let account: AccountResponse = resp
        .json()
        .with_context(|| format!("Failed to parse the GitHub App bot account {login}"))?;
    let account = validated(&account, "the GitHub App bot account")?;
    if account.login != login {
        bail!(
            "GitHub returned account {} for App slug {slug}; expected {login}",
            account.login
        );
    }
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_mock::{MockHttpClient, test_rsa_key};

    fn app(slug: Option<&str>) -> GitHubAppCredentials {
        GitHubAppCredentials {
            app_id:          "12345".to_string(),
            private_key_pem: test_rsa_key().to_string(),
            slug:            slug.map(str::to_string),
        }
    }

    #[test]
    fn noreply_email_uses_the_hosts_domain() {
        let identity = GitHubAccountIdentity {
            login: "octo".to_string(),
            id:    7,
        };
        let ghes = GitHubHost::new("https://ghe.example.com", None).unwrap();
        assert_eq!(
            identity.noreply_email(&ghes),
            "7+octo@users.noreply.ghe.example.com"
        );
        let ghec = GitHubHost::new("https://octocorp.ghe.com", None).unwrap();
        assert_eq!(
            identity.noreply_email(&ghec),
            "7+octo@users.noreply.octocorp.ghe.com"
        );
    }

    #[test]
    fn noreply_email_uses_id_plus_login() {
        let identity = GitHubAccountIdentity {
            login: "fabro-sh[bot]".to_string(),
            id:    281_434_857,
        };
        assert_eq!(
            identity.noreply_email(&GitHubHost::default()),
            "281434857+fabro-sh[bot]@users.noreply.github.com"
        );
    }

    #[tokio::test]
    async fn token_identity_reads_login_and_id_with_bearer_auth() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/user",
                200,
                r#"{"login":"octocat","id":583231,"type":"User","email":null}"#,
            )
            .with_req_header("Authorization", "Bearer ghp_secret");

        let identity = lookup_token_identity(&mock, "ghp_secret", "")
            .await
            .unwrap();
        assert_eq!(identity, GitHubAccountIdentity {
            login: "octocat".to_string(),
            id:    583_231,
        });
        assert_eq!(mock.request_count(), 1);
    }

    #[tokio::test]
    async fn token_identity_rejects_unauthorized_tokens() {
        let mock = MockHttpClient::new().on(HttpMethod::Get, "/user", 401, "{}");
        let err = lookup_token_identity(&mock, "ghp_bad", "")
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("rejected the configured GITHUB_TOKEN"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn token_identity_rejects_malformed_bodies() {
        let mock = MockHttpClient::new().on(HttpMethod::Get, "/user", 200, r#"{"login":""}"#);
        let err = lookup_token_identity(&mock, "ghp_x", "").await.unwrap_err();
        let chain = err.chain().map(ToString::to_string).collect::<Vec<_>>();
        assert!(
            chain
                .iter()
                .any(|message| message.contains("Failed to parse the GitHub token's user")),
            "{chain:?}"
        );

        let mock =
            MockHttpClient::new().on(HttpMethod::Get, "/user", 200, r#"{"login":"  ","id":7}"#);
        let err = lookup_token_identity(&mock, "ghp_x", "").await.unwrap_err();
        assert!(err.to_string().contains("empty login"), "{err:#}");

        let mock = MockHttpClient::new().on(
            HttpMethod::Get,
            "/user",
            200,
            r#"{"login":"evil\nname","id":7}"#,
        );
        let err = lookup_token_identity(&mock, "ghp_x", "").await.unwrap_err();
        assert!(err.to_string().contains("invalid login"), "{err:#}");
    }

    #[tokio::test]
    async fn token_identity_surfaces_transient_failures() {
        let mock = MockHttpClient::new().on(HttpMethod::Get, "/user", 502, "bad gateway");
        let err = lookup_token_identity(&mock, "ghp_x", "").await.unwrap_err();
        assert!(err.to_string().contains("Unexpected status 502"), "{err:#}");
    }

    #[tokio::test]
    async fn app_bot_identity_uses_configured_slug_and_bearer() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/users/my-app%5Bbot%5D",
                200,
                r#"{"login":"my-app[bot]","id":4242,"type":"Bot"}"#,
            )
            .with_req_header("Authorization", "Bearer ghs_installation");

        let identity =
            lookup_app_bot_identity(&mock, &app(Some("my-app")), "", Some("ghs_installation"))
                .await
                .unwrap();
        assert_eq!(identity.login, "my-app[bot]");
        assert_eq!(identity.id, 4242);
        assert_eq!(
            identity.noreply_email(&GitHubHost::default()),
            "4242+my-app[bot]@users.noreply.github.com"
        );
        // The configured slug skips the `/app` lookup entirely.
        assert_eq!(mock.request_count(), 1);
    }

    #[tokio::test]
    async fn app_bot_identity_discovers_the_slug_when_not_configured() {
        let mock = MockHttpClient::new()
            .on(
                HttpMethod::Get,
                "/app",
                200,
                r#"{"slug":"discovered-app","owner":{"login":"org"}}"#,
            )
            .on(
                HttpMethod::Get,
                "/users/discovered-app%5Bbot%5D",
                200,
                r#"{"login":"discovered-app[bot]","id":99}"#,
            );

        let identity = lookup_app_bot_identity(&mock, &app(None), "", None)
            .await
            .unwrap();
        assert_eq!(identity.login, "discovered-app[bot]");
        assert_eq!(mock.request_count(), 2);
    }

    #[tokio::test]
    async fn app_bot_identity_fails_when_the_bot_account_is_missing() {
        let mock = MockHttpClient::new().on(HttpMethod::Get, "/users/nope%5Bbot%5D", 404, "{}");
        let err = lookup_app_bot_identity(&mock, &app(Some("nope")), "", None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("no bot account for App slug nope"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn app_bot_identity_rejects_a_mismatched_login() {
        let mock = MockHttpClient::new().on(
            HttpMethod::Get,
            "/users/my-app%5Bbot%5D",
            200,
            r#"{"login":"someone-else","id":5}"#,
        );
        let err = lookup_app_bot_identity(&mock, &app(Some("my-app")), "", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("expected my-app[bot]"), "{err:#}");
    }

    #[tokio::test]
    async fn app_bot_identity_preserves_the_app_lookup_error_chain() {
        let mock = MockHttpClient::new().on(HttpMethod::Get, "/app", 401, "");
        let err = lookup_app_bot_identity(&mock, &app(None), "", None)
            .await
            .unwrap_err();
        let chain = err.chain().map(ToString::to_string).collect::<Vec<_>>();
        assert!(
            chain
                .iter()
                .any(|m| m.contains("Failed to determine the GitHub App slug")),
            "{chain:?}"
        );
        assert!(
            chain.iter().any(|m| m.contains("authentication failed")),
            "{chain:?}"
        );
    }
}
